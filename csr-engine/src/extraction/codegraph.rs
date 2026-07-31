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
        // language keywords misparsed as callees (WCR Phase 7, TASK D): a
        // dynamic `import('./mod')` is a `call_expression` whose `function`
        // field is the literal keyword token `import`, not a symbol
        // reference — there is no def anywhere for it to bind to, by
        // construction of the language grammar itself, so classifying it as
        // a real callee only manufactures unexplained edges.
        "import",
    ]
    .into_iter()
    .collect()
});

/// A callee/import name with no provenance value: single char (closure params
/// like `r`/`e`, generics like `T`) or a known language built-in.
fn is_noise_callee(name: &str) -> bool {
    name.len() < 2 || NOISE_CALLEES.contains(name)
}

/// Max length of a module specifier recorded in an `imports` edge's
/// `evidence` field (`from:<module>`), matching the WCR spec's plausibility
/// backstop for identifier-shaped keys elsewhere in the gate.
const MODULE_EVIDENCE_MAX_LEN: usize = 120;

/// Truncate `s` to at most `max` **characters** (not bytes) — safe for
/// multi-byte UTF-8 module specifiers, unlike a raw byte-index slice.
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// True when a call's `function` field node kind denotes a receiver-based
/// (method/field) callee rather than a bare identifier or path. Checked BEFORE
/// `bare_callee()` strips the receiver, since the stripped text can't tell the
/// two apart. Rust path calls (`mod::foo()`) fall through to "direct" — they
/// have no runtime receiver, just a namespace qualifier.
fn is_method_callee_kind(kind: &str) -> bool {
    matches!(
        kind,
        "field_expression"      // Rust: r.map_err(f)
            | "member_expression" // TS/JS: obj.fetch()
            | "attribute"          // Python: self.run()
            | "selector_expression" // Go: obj.Method()
    )
}

/// Extract a "qualifier" path preceding the final callee segment, when
/// syntactically trivial and worth recording as `via:<qualifier>` calls-edge
/// evidence for the resolver's qualifier-aware classification (WCR Phase 6,
/// TASK A/B — see `extraction::resolver::qualifier_tier`). Checked BEFORE
/// `bare_callee()` strips the qualifier away, same reasoning as
/// `is_method_callee_kind`.
///
/// - Rust `scoped_identifier` (namespace-path calls: `Instant::now()`,
///   `fs::read_to_string()`, `std::mem::swap(a, b)`) — the AST `path` field,
///   everything before the final `::segment`, via the same field-based
///   approach `rust_use_path_symbols` uses for `use` statements.
/// - Python `attribute` (`json.dumps(...)`, `os.path.abspath(...)`, and also
///   `self.run()` / `self.x.run()`) — the AST `object` field, everything
///   before the final `.attr`. Captured even though Python attribute calls
///   are already `callee_kind = "method"` — the qualifier lets the resolver
///   recognize a module-qualified call (`json.dumps`) as X1 `external`
///   rather than the vaguer X2 `method` (receiver-call) bucket.
/// - TS/JS `member_expression` (`obj.fetch()`) — the `object` field's raw
///   text, but only when it is a "trivial" dotted identifier chain (ASCII
///   letters/digits/`_`/`.` only). Anything with call parens, brackets, or
///   other punctuation (`(a || b).x()`, `arr[i].x()`) is a shape we don't
///   try to summarize — skipped rather than guessed at.
///
/// Returns `None` for a bare (unqualified) call or any other kind/language
/// combination — the caller then falls back to no `via:` evidence, exactly
/// today's behavior.
fn call_qualifier<D: ast_grep_core::Doc>(
    func: &ast_grep_core::Node<'_, D>,
    lang: SupportLang,
) -> Option<String> {
    match lang {
        SupportLang::Rust if func.kind().as_ref() == "scoped_identifier" => {
            func.field("path").map(|p| p.text().to_string())
        }
        SupportLang::Python if func.kind().as_ref() == "attribute" => {
            func.field("object").map(|o| o.text().to_string())
        }
        SupportLang::TypeScript | SupportLang::Tsx | SupportLang::JavaScript
            if func.kind().as_ref() == "member_expression" =>
        {
            let object = func.field("object")?;
            let text = object.text().to_string();
            let trivial = !text.is_empty()
                && text
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '.');
            if trivial {
                Some(text)
            } else {
                None
            }
        }
        _ => None,
    }
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

/// Extract imported local binding names from an import AST node, paired with
/// the module specifier they were imported from (Rust `use` path prefix, the
/// JS/TS import source string, the Python module, the Go import path — see
/// each per-language helper's doc comment). The module half is `""` when no
/// module context is derivable (e.g. bare `use std;` — `std` IS the module,
/// there is no prefix before it).
///
/// Uses tree-sitter grammar fields so multi-symbol imports become one edge
/// per symbol (e.g. `use std::sync::{Arc, OnceLock}` -> `["Arc", "OnceLock"]`)
/// rather than a concatenated blob from the whole statement text.
fn import_symbols<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
    lang: SupportLang,
) -> Vec<(String, String)> {
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

fn push_symbol(out: &mut Vec<(String, String)>, symbol: impl AsRef<str>, module: impl AsRef<str>) {
    let s = symbol.as_ref().trim();
    if !s.is_empty() {
        out.push((s.to_string(), module.as_ref().trim().to_string()));
    }
}

/// Strip surrounding quotes (`'`, `"`, `` ` ``) from a raw string-literal token.
fn unquote(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c| c == '"' || c == '`')
        .trim_matches('\'')
        .to_string()
}

/// Rust `use_declaration`: field `argument`, then recurse the use-path tree,
/// threading the accumulated module prefix (everything before the final
/// path segment) down to each leaf symbol. `use std::collections::HashMap;`
/// yields `("HashMap", "std::collections")`; `use std;` yields
/// `("std", "")` — there is no prefix, `std` itself is the module.
fn rust_import_symbols<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(arg) = node.field("argument") {
        rust_use_path_symbols(&arg, None, &mut out);
    }
    out
}

fn rust_use_path_symbols<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
    inherited_module: Option<&str>,
    out: &mut Vec<(String, String)>,
) {
    match node.kind().as_ref() {
        "identifier" => {
            push_symbol(out, node.text(), inherited_module.unwrap_or(""));
        }
        "scoped_identifier" => {
            if let Some(name) = node.field("name") {
                // field("path") is everything before `::name` — the module
                // prefix for this leaf directly, no manual reconstruction.
                let module = node
                    .field("path")
                    .map(|p| p.text().to_string())
                    .or_else(|| inherited_module.map(str::to_string))
                    .unwrap_or_default();
                push_symbol(out, name.text(), module);
            }
        }
        "scoped_use_list" => {
            let module = node.field("path").map(|p| p.text().to_string());
            if let Some(list) = node.field("list") {
                rust_use_path_symbols(&list, module.as_deref().or(inherited_module), out);
            }
        }
        "use_list" => {
            for child in node.children() {
                if child.is_named() {
                    rust_use_path_symbols(&child, inherited_module, out);
                }
            }
        }
        "use_as_clause" => {
            if let Some(alias) = node.field("alias") {
                // field("path") is the full aliased path (e.g. `foo::Bar` for
                // `use foo::Bar as Baz;`); its own field("path") — one level
                // deeper — is the prefix before the final segment (`foo`).
                let module = node
                    .field("path")
                    .and_then(|p| p.field("path"))
                    .map(|inner| inner.text().to_string())
                    .unwrap_or_default();
                push_symbol(out, alias.text(), module);
            }
        }
        "use_wildcard" => {
            // `use foo::*;` — no local binding name to track.
        }
        _ => {}
    }
}

/// JS/TS `import_statement`: field `source` holds the module specifier
/// string literal, shared by every binding in the statement; non-field child
/// `import_clause` holds the bindings themselves.
fn js_ts_import_symbols<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let module = node
        .field("source")
        .map(|s| unquote(&s.text()))
        .unwrap_or_default();
    for child in node.children() {
        if !child.is_named() || child.kind().as_ref() != "import_clause" {
            continue;
        }
        for part in child.children() {
            if !part.is_named() {
                continue;
            }
            match part.kind().as_ref() {
                "identifier" => push_symbol(&mut out, part.text(), &module),
                "named_imports" => {
                    for spec in part.children() {
                        if !spec.is_named() || spec.kind().as_ref() != "import_specifier" {
                            continue;
                        }
                        if let Some(alias) = spec.field("alias") {
                            push_symbol(&mut out, alias.text(), &module);
                        } else if let Some(name) = spec.field("name") {
                            push_symbol(&mut out, name.text(), &module);
                        }
                    }
                }
                "namespace_import" => {
                    for id in part.children() {
                        if id.is_named() && id.kind().as_ref() == "identifier" {
                            push_symbol(&mut out, id.text(), &module);
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

/// Python `import_statement` / `import_from_statement`. For `from X import
/// a, b as c`, `field("module_name")` (`X`) is the module shared by every
/// bound name. For a bare `import a.b.c` / `import a.b.c as d`, there is no
/// `from` module — the dotted path being imported (before any alias) IS the
/// module.
fn python_import_symbols<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    match node.kind().as_ref() {
        "import_statement" => {
            // `name` is multiple; `.field("name")` would only return the first.
            for name_node in node.field_children("name") {
                python_import_name(&name_node, None, &mut out);
            }
        }
        "import_from_statement" => {
            let module = node.field("module_name").map(|m| m.text().to_string());
            // `wildcard_import` is a non-field child and emits nothing.
            for name_node in node.field_children("name") {
                python_import_name(&name_node, module.as_deref(), &mut out);
            }
        }
        _ => {}
    }
    out
}

fn python_import_name<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
    from_module: Option<&str>,
    out: &mut Vec<(String, String)>,
) {
    match node.kind().as_ref() {
        "aliased_import" => {
            if let Some(alias) = node.field("alias") {
                // Under `from X import Y as Z`, `from_module` (`X`) wins.
                // Under bare `import Y as Z`, there is no from-module — the
                // aliased name's own dotted path is the module.
                let module = from_module.map(str::to_string).unwrap_or_else(|| {
                    node.field("name")
                        .map(|n| n.text().to_string())
                        .unwrap_or_default()
                });
                push_symbol(out, alias.text(), module);
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
                let module = from_module
                    .map(str::to_string)
                    .unwrap_or_else(|| node.text().to_string());
                push_symbol(out, name, module);
            }
        }
        _ => {}
    }
}

/// Go `import_declaration` -> `import_spec` / `import_spec_list`. The module
/// is the full (unquoted) import path, e.g. `github.com/foo/bar`.
fn go_import_symbols<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
) -> Vec<(String, String)> {
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

fn go_import_spec<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
    out: &mut Vec<(String, String)>,
) {
    let module = node
        .field("path")
        .map(|p| unquote(&p.text()))
        .unwrap_or_default();
    if let Some(name) = node.field("name") {
        push_symbol(out, name.text(), &module);
        return;
    }
    if !module.is_empty() {
        if let Some(seg) = module.rsplit('/').next() {
            push_symbol(out, seg, &module);
        }
    }
}

/// TS/JS/TSX top-level `const`/`let`/`var` declaration AST kinds (WCR Phase
/// 7, TASK C): `lexical_declaration` covers both `const` and `let`,
/// `variable_declaration` covers `var`. No other language: Rust has no
/// module-level mutable/const-binding equivalent worth indexing the same
/// way (a `const`/`static` item is already a distinct AST kind this
/// extractor doesn't currently walk, out of scope here), and Python/Go
/// module-level assignments have no dedicated declaration-kind wrapper node
/// to anchor a program-level check on.
fn const_decl_kinds(lang: SupportLang) -> &'static [&'static str] {
    match lang {
        SupportLang::TypeScript | SupportLang::Tsx | SupportLang::JavaScript => {
            &["lexical_declaration", "variable_declaration"]
        }
        _ => &[],
    }
}

/// True when `node` (a `lexical_declaration`/`variable_declaration`) is a
/// direct program-level statement — a bare top-level `const X = ...;`, or
/// one wrapped in `export_statement` (`export const X = ...;`) that is
/// itself a direct child of `program`. A `const` nested inside a function
/// body, block, or class is local scope, not a repo-wide symbol worth
/// indexing the same way a def node is (WCR Phase 7, TASK C).
fn is_program_level_declaration<D: ast_grep_core::Doc>(node: &ast_grep_core::Node<'_, D>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind().as_ref() {
        "program" => true,
        "export_statement" => parent
            .parent()
            .is_some_and(|grandparent| grandparent.kind().as_ref() == "program"),
        _ => false,
    }
}

/// Simple-identifier binding names declared by a `lexical_declaration`/
/// `variable_declaration` (WCR Phase 7, TASK C): one name per
/// `variable_declarator` child whose `name` field is a plain `identifier`.
/// Destructuring patterns (`const {a, b} = obj`, `const [a, b] = arr`) are
/// deliberately skipped — there is no single declared symbol name to anchor
/// a def node on, and guessing at one from the pattern shape would violate
/// the evidence-only rule.
fn const_decl_names<D: ast_grep_core::Doc>(node: &ast_grep_core::Node<'_, D>) -> Vec<String> {
    let mut out = Vec::new();
    for child in node.children() {
        if !child.is_named() || child.kind().as_ref() != "variable_declarator" {
            continue;
        }
        if let Some(name_node) = child.field("name") {
            if name_node.kind().as_ref() == "identifier" {
                out.push(name_node.text().to_string());
            }
        }
    }
    out
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

    // `callee_kind` is '' for defines/imports edges; 'direct'/'method' for calls.
    // `evidence` is '' for defines edges; `from:<module>` (<=120 chars, may be
    // '' when no module is derivable) for imports edges; `via:<qualifier>`
    // (<=120 chars, may be '' when the call has no capturable qualifier —
    // WCR Phase 6, TASK A) for calls edges — captured at extraction time so
    // the resolver's X1 tier can classify boundaries by module/qualifier
    // rather than degraded bound-symbol-name matching.
    let mut add_edge =
        |src: String, dst: String, kind: &str, resolved: i64, callee_kind: &str, evidence: &str| {
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
                    callee_kind: callee_kind.into(),
                    boundary: String::new(),
                    evidence: evidence.into(),
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
                    add_edge(module_id.clone(), nid, "defines", 1, "", "");
                }
            }
        }
    }

    // TS/JS/TSX top-level const/let/var declarations -> def nodes kind
    // 'const' (WCR Phase 7, TASK C). Only program-level (see
    // `is_program_level_declaration`) — a local declaration inside a
    // function body is not a repo-wide symbol. Destructuring patterns are
    // skipped (see `const_decl_names`).
    for kind in const_decl_kinds(lang) {
        let matcher = KindMatcher::new(kind, lang);
        for n in root.find_all(&matcher) {
            if !is_program_level_declaration(&n) {
                continue;
            }
            for name in const_decl_names(&n) {
                if name.len() < 2 {
                    continue;
                }
                let text = n.text().to_string();
                let start = n.start_pos().line() as i64;
                let end = start + text.lines().count().saturating_sub(1) as i64;
                let node = mk_node("const", &name, &text, start, end);
                let nid = node.id.clone();
                nodes.insert(nid.clone(), node);
                add_edge(module_id.clone(), nid, "defines", 1, "", "");
            }
        }
    }

    // Imports -> imports edge from module (placeholder dst per imported symbol).
    for kind in import_kinds(lang) {
        let matcher = KindMatcher::new(kind, lang);
        for n in root.find_all(&matcher) {
            for (sym, module) in import_symbols(&n, lang) {
                if is_noise_callee(&sym) {
                    continue;
                }
                let evidence = if module.is_empty() {
                    String::new()
                } else {
                    format!("from:{}", truncate_chars(&module, MODULE_EVIDENCE_MAX_LEN))
                };
                add_edge(
                    module_id.clone(),
                    format!("name:{sym}"),
                    "imports",
                    0,
                    "",
                    &evidence,
                );
            }
        }
    }

    // Call sites -> calls edge from enclosing def (placeholder dst).
    for kind in call_kinds(lang) {
        let matcher = KindMatcher::new(kind, lang);
        for n in root.find_all(&matcher) {
            let func = match n.field("function") {
                Some(f) => f,
                None => continue,
            };
            // Capture the kind BEFORE bare_callee() strips the receiver — the
            // stripped text alone can't distinguish `obj.foo()` from `foo()`.
            let callee_kind = if is_method_callee_kind(func.kind().as_ref()) {
                "method"
            } else {
                "direct"
            };
            // Captured BEFORE bare_callee() strips the qualifier away — see
            // call_qualifier's doc comment (WCR Phase 6, TASK A).
            let qualifier = call_qualifier(&func, lang);
            let callee = match bare_callee(&func.text()) {
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
            let evidence = match qualifier {
                Some(q) if !q.is_empty() => {
                    format!("via:{}", truncate_chars(&q, MODULE_EVIDENCE_MAX_LEN))
                }
                _ => String::new(),
            };
            add_edge(
                src,
                format!("name:{callee}"),
                "calls",
                0,
                callee_kind,
                &evidence,
            );
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
    fn callee_kind_rust_method_vs_direct() {
        // `map_err` is a receiver-based (method) call — must be classified 'method'.
        let method_src = "fn foo() {\n    let r: Result<(), ()> = Err(());\n    let _ = r.map_err(handler);\n}\nfn handler(_e: ()) {}\n";
        let frag = extract_graph_fragment(
            method_src,
            SupportLang::Rust,
            "a.rs",
            "repo",
            "proj",
            "c",
            "s",
        );
        let kind = frag
            .edges
            .iter()
            .find(|e| e.kind == "calls" && e.dst_id == "name:map_err")
            .expect("map_err calls edge present")
            .callee_kind
            .clone();
        assert_eq!(kind, "method", "receiver call must be classified method");

        // `helper()` is a bare identifier call — must be classified 'direct'.
        let direct_src = "fn foo() {\n    helper();\n}\nfn helper() {}\n";
        let frag2 = extract_graph_fragment(
            direct_src,
            SupportLang::Rust,
            "a.rs",
            "repo",
            "proj",
            "c",
            "s",
        );
        let kind2 = frag2
            .edges
            .iter()
            .find(|e| e.kind == "calls" && e.dst_id == "name:helper")
            .expect("helper calls edge present")
            .callee_kind
            .clone();
        assert_eq!(
            kind2, "direct",
            "bare identifier call must be classified direct"
        );
    }

    #[test]
    fn callee_kind_typescript_member_vs_bare() {
        let member_src = "function run() {\n    obj.fetch();\n}\n";
        let frag = extract_graph_fragment(
            member_src,
            SupportLang::TypeScript,
            "a.ts",
            "repo",
            "proj",
            "c",
            "s",
        );
        let kind = frag
            .edges
            .iter()
            .find(|e| e.kind == "calls" && e.dst_id == "name:fetch")
            .expect("fetch calls edge present")
            .callee_kind
            .clone();
        assert_eq!(kind, "method", "obj.fetch() must be classified method");

        let bare_src = "function run() {\n    fetch();\n}\n";
        let frag2 = extract_graph_fragment(
            bare_src,
            SupportLang::TypeScript,
            "a.ts",
            "repo",
            "proj",
            "c",
            "s",
        );
        let kind2 = frag2
            .edges
            .iter()
            .find(|e| e.kind == "calls" && e.dst_id == "name:fetch")
            .expect("fetch calls edge present")
            .callee_kind
            .clone();
        assert_eq!(kind2, "direct", "bare fetch() must be classified direct");
    }

    #[test]
    fn callee_kind_python_method_call() {
        let src = "class Foo:\n    def handler(self):\n        self.run()\n";
        let frag =
            extract_graph_fragment(src, SupportLang::Python, "a.py", "repo", "proj", "c", "s");
        let kind = frag
            .edges
            .iter()
            .find(|e| e.kind == "calls" && e.dst_id == "name:run")
            .expect("run calls edge present")
            .callee_kind
            .clone();
        assert_eq!(kind, "method", "self.run() must be classified method");
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
        let mut raw: Vec<(String, String)> = Vec::new();
        for kind in import_kinds(SupportLang::Python) {
            let matcher = KindMatcher::new(kind, SupportLang::Python);
            for n in root.find_all(&matcher) {
                raw.extend(import_symbols(&n, SupportLang::Python));
            }
        }
        assert!(
            raw.iter().any(|(s, m)| s == "join" && m == "os.path"),
            "AST extraction must yield join from os.path: {raw:?}"
        );
        assert!(
            raw.iter().any(|(s, m)| s == "exists" && m == "os.path"),
            "AST extraction must yield exists from os.path: {raw:?}"
        );
        assert!(
            raw.iter().any(|(s, m)| s == "np" && m == "numpy"),
            "AST extraction must yield np from numpy: {raw:?}"
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

    // ─── Module specifier capture (WCR Phase 5, TASK 1) ───

    fn import_evidence<'a>(frag: &'a GraphFragment, dst: &str) -> &'a str {
        frag.edges
            .iter()
            .find(|e| e.kind == "imports" && e.dst_id == dst)
            .unwrap_or_else(|| panic!("no imports edge for {dst}"))
            .evidence
            .as_str()
    }

    #[test]
    fn rust_import_evidence_records_module_path_prefix() {
        let frag = extract_graph_fragment(
            "use std::collections::HashMap;\n",
            SupportLang::Rust,
            "a.rs",
            "repo",
            "proj",
            "c",
            "s",
        );
        assert_eq!(
            import_evidence(&frag, "name:HashMap"),
            "from:std::collections"
        );
    }

    #[test]
    fn rust_import_evidence_multi_symbol_use_shares_module() {
        let frag = extract_graph_fragment(
            "use std::sync::{Arc, OnceLock};\n",
            SupportLang::Rust,
            "a.rs",
            "repo",
            "proj",
            "c",
            "s",
        );
        assert_eq!(import_evidence(&frag, "name:Arc"), "from:std::sync");
        assert_eq!(import_evidence(&frag, "name:OnceLock"), "from:std::sync");
    }

    #[test]
    fn rust_import_evidence_alias_records_prefix_before_original_name() {
        let frag = extract_graph_fragment(
            "use foo::Bar as Baz;\n",
            SupportLang::Rust,
            "a.rs",
            "repo",
            "proj",
            "c",
            "s",
        );
        assert_eq!(import_evidence(&frag, "name:Baz"), "from:foo");
    }

    #[test]
    fn rust_import_evidence_bare_crate_has_no_module_prefix() {
        // `use std;` — `std` IS the module, there is no prefix before it, so
        // evidence stays empty rather than recording a meaningless `from:`.
        let frag = extract_graph_fragment(
            "use std;\n",
            SupportLang::Rust,
            "a.rs",
            "repo",
            "proj",
            "c",
            "s",
        );
        assert_eq!(import_evidence(&frag, "name:std"), "");
    }

    #[test]
    fn typescript_import_evidence_records_source_string() {
        let frag = extract_graph_fragment(
            "import fs from 'fs';\nimport { fileURLToPath } from 'url';\nimport * as path from 'path';\nimport { helper } from './util';\n",
            SupportLang::TypeScript,
            "a.ts",
            "repo",
            "proj",
            "c",
            "s",
        );
        assert_eq!(import_evidence(&frag, "name:fs"), "from:fs");
        assert_eq!(import_evidence(&frag, "name:fileURLToPath"), "from:url");
        assert_eq!(import_evidence(&frag, "name:path"), "from:path");
        assert_eq!(import_evidence(&frag, "name:helper"), "from:./util");
    }

    #[test]
    fn python_import_evidence_records_full_dotted_module() {
        let frag = extract_graph_fragment(
            "from os.path import exists\nimport numpy as np\n",
            SupportLang::Python,
            "a.py",
            "repo",
            "proj",
            "c",
            "s",
        );
        assert_eq!(import_evidence(&frag, "name:exists"), "from:os.path");
        assert_eq!(import_evidence(&frag, "name:np"), "from:numpy");
    }

    #[test]
    fn go_import_evidence_records_full_import_path() {
        // Alias must be >=2 chars: `is_noise_callee` drops single-char names
        // (closure-param-style noise), so a `j "encoding/json"` alias would
        // never reach edge emission at all.
        let src = "package main\n\nimport (\n\t\"fmt\"\n\tjs \"encoding/json\"\n)\n\nfunc main() {\n\tfmt.Println(js)\n}\n";
        let frag = extract_graph_fragment(src, SupportLang::Go, "a.go", "repo", "proj", "c", "s");
        assert_eq!(import_evidence(&frag, "name:fmt"), "from:fmt");
        assert_eq!(import_evidence(&frag, "name:js"), "from:encoding/json");
    }

    #[test]
    fn import_module_evidence_is_capped_at_120_chars() {
        let long_segment = "a".repeat(200);
        let src = format!("import {{ thing }} from '{long_segment}';\n");
        let frag = extract_graph_fragment(
            &src,
            SupportLang::TypeScript,
            "a.ts",
            "repo",
            "proj",
            "c",
            "s",
        );
        let evidence = import_evidence(&frag, "name:thing");
        // "from:" (5 chars) + at most 120 chars of module.
        assert!(
            evidence.len() <= 5 + 120,
            "evidence must be capped: {} chars: {evidence}",
            evidence.len()
        );
        assert!(evidence.starts_with("from:aaaa"));
    }

    // ─── Call-site qualifier capture (WCR Phase 6, TASK A) ───

    fn call_evidence<'a>(frag: &'a GraphFragment, dst: &str) -> &'a str {
        frag.edges
            .iter()
            .find(|e| e.kind == "calls" && e.dst_id == dst)
            .unwrap_or_else(|| panic!("no calls edge for {dst}"))
            .evidence
            .as_str()
    }

    #[test]
    fn rust_scoped_call_captures_qualifier_as_via_evidence() {
        let src = "fn foo() {\n    let _ = std::time::Instant::now();\n    let _ = fs::read_to_string(\"x\");\n}\n";
        let frag = extract_graph_fragment(src, SupportLang::Rust, "a.rs", "repo", "proj", "c", "s");
        assert_eq!(
            call_evidence(&frag, "name:now"),
            "via:std::time::Instant",
            "qualifier is everything before the final ::segment"
        );
        assert_eq!(call_evidence(&frag, "name:read_to_string"), "via:fs");
    }

    #[test]
    fn rust_bare_call_has_no_qualifier_and_stays_direct() {
        let src = "fn foo() {\n    helper();\n}\nfn helper() {}\n";
        let frag = extract_graph_fragment(src, SupportLang::Rust, "a.rs", "repo", "proj", "c", "s");
        let edge = frag
            .edges
            .iter()
            .find(|e| e.kind == "calls" && e.dst_id == "name:helper")
            .unwrap();
        assert_eq!(edge.evidence, "");
        assert_eq!(edge.callee_kind, "direct", "callee_kind stays as-is");
    }

    #[test]
    fn rust_method_call_does_not_capture_a_via_qualifier() {
        // `r.map_err(f)` is a field_expression (method) callee, not a
        // scoped_identifier — TASK A only captures qualifiers for path calls.
        let src = "fn foo() {\n    let r: Result<(), ()> = Err(());\n    let _ = r.map_err(handler);\n}\nfn handler(_e: ()) {}\n";
        let frag = extract_graph_fragment(src, SupportLang::Rust, "a.rs", "repo", "proj", "c", "s");
        let edge = frag
            .edges
            .iter()
            .find(|e| e.kind == "calls" && e.dst_id == "name:map_err")
            .unwrap();
        assert_eq!(edge.evidence, "");
        assert_eq!(edge.callee_kind, "method");
    }

    #[test]
    fn python_attribute_call_captures_dotted_receiver_as_via_evidence() {
        let src = "import json\nimport os.path\n\ndef handler():\n    json.dumps({})\n    os.path.abspath(\".\")\n    self.run()\n";
        let frag =
            extract_graph_fragment(src, SupportLang::Python, "a.py", "repo", "proj", "c", "s");
        assert_eq!(call_evidence(&frag, "name:dumps"), "via:json");
        assert_eq!(call_evidence(&frag, "name:abspath"), "via:os.path");
        assert_eq!(call_evidence(&frag, "name:run"), "via:self");
        // Python attribute calls are still classified 'method' by callee_kind
        // — capturing a qualifier does not change that (TASK A leaves
        // callee_kind as-is; the resolver's qualifier tier is what changes
        // behavior downstream, see resolver::qualifier_tier).
        let run_edge = frag
            .edges
            .iter()
            .find(|e| e.kind == "calls" && e.dst_id == "name:run")
            .unwrap();
        assert_eq!(run_edge.callee_kind, "method");
    }

    #[test]
    fn typescript_trivial_member_call_captures_object_path_as_via_evidence() {
        // `x.y.z()` — not `a.b.c()`: single-char trailing names (`c`) are
        // dropped by is_noise_callee's len<2 rule regardless of TASK A.
        let src = "function run() {\n    obj.fetch();\n    x.y.helper_call();\n}\n";
        let frag = extract_graph_fragment(
            src,
            SupportLang::TypeScript,
            "a.ts",
            "repo",
            "proj",
            "c",
            "s",
        );
        assert_eq!(call_evidence(&frag, "name:fetch"), "via:obj");
        assert_eq!(call_evidence(&frag, "name:helper_call"), "via:x.y");
    }

    #[test]
    fn typescript_non_trivial_member_call_has_no_via_evidence() {
        // `(a || b).x()` — the object is not a simple dotted identifier
        // chain; TASK A deliberately skips it rather than guessing.
        let src = "function run() {\n    (a || b).nontrivial_method();\n}\n";
        let frag = extract_graph_fragment(
            src,
            SupportLang::TypeScript,
            "a.ts",
            "repo",
            "proj",
            "c",
            "s",
        );
        let edge = frag
            .edges
            .iter()
            .find(|e| e.kind == "calls" && e.dst_id == "name:nontrivial_method");
        if let Some(edge) = edge {
            assert_eq!(edge.evidence, "", "non-trivial object must not be captured");
        }
    }

    // ─── Dynamic import() keyword noise (WCR Phase 7, TASK D) ───

    #[test]
    fn dynamic_import_call_is_not_emitted_as_a_calls_edge() {
        let src =
            "async function run() {\n    const mod = await import('./lazy');\n    return mod;\n}\n";
        let frag = extract_graph_fragment(
            src,
            SupportLang::TypeScript,
            "a.ts",
            "repo",
            "proj",
            "c",
            "s",
        );
        let call_dsts: Vec<&str> = frag
            .edges
            .iter()
            .filter(|e| e.kind == "calls")
            .map(|e| e.dst_id.as_str())
            .collect();
        assert!(
            !call_dsts.contains(&"name:import"),
            "dynamic import() keyword must not be emitted as a calls edge: {call_dsts:?}"
        );
    }

    #[test]
    fn via_evidence_is_capped_at_120_chars() {
        let long_segment = "a".repeat(200);
        let src = format!("fn foo() {{\n    {long_segment}::helper_call_fn();\n}}\n");
        let frag =
            extract_graph_fragment(&src, SupportLang::Rust, "a.rs", "repo", "proj", "c", "s");
        let evidence = call_evidence(&frag, "name:helper_call_fn");
        assert!(
            evidence.len() <= 4 + 120,
            "evidence must be capped: {} chars: {evidence}",
            evidence.len()
        );
        assert!(evidence.starts_with("via:aaaa"));
    }

    // ─── Top-level const/let/var def nodes (WCR Phase 7, TASK C) ───

    fn def_names_of_kind<'a>(frag: &'a GraphFragment, kind: &str) -> Vec<&'a str> {
        frag.nodes
            .iter()
            .filter(|n| n.kind == kind)
            .map(|n| n.name.as_str())
            .collect()
    }

    #[test]
    fn typescript_top_level_const_becomes_a_def_node() {
        let src = "export const COLORS = { primary: 'blue' };\nconst AnalyticsEvents = 1;\n";
        let frag = extract_graph_fragment(
            src,
            SupportLang::TypeScript,
            "a.ts",
            "repo",
            "proj",
            "c",
            "s",
        );
        let names = def_names_of_kind(&frag, "const");
        assert!(names.contains(&"COLORS"), "names: {names:?}");
        assert!(names.contains(&"AnalyticsEvents"), "names: {names:?}");

        // A defines edge from the module anchors each const def, same as a
        // function/type def.
        let colors_id = node_id("repo", "a.ts", "const", "COLORS");
        assert!(frag
            .edges
            .iter()
            .any(|e| e.kind == "defines" && e.dst_id == colors_id));
    }

    #[test]
    fn typescript_top_level_let_and_var_become_def_nodes() {
        let src = "let styles = {};\nvar legacyThing = 1;\n";
        let frag = extract_graph_fragment(
            src,
            SupportLang::TypeScript,
            "a.ts",
            "repo",
            "proj",
            "c",
            "s",
        );
        let names = def_names_of_kind(&frag, "const");
        assert!(names.contains(&"styles"), "names: {names:?}");
        assert!(names.contains(&"legacyThing"), "names: {names:?}");
    }

    #[test]
    fn javascript_and_tsx_top_level_const_becomes_a_def_node() {
        for lang in [SupportLang::JavaScript, SupportLang::Tsx] {
            let src = "export const widgetConfig = 1;\n";
            let frag = extract_graph_fragment(src, lang, "a.tsx", "repo", "proj", "c", "s");
            let names = def_names_of_kind(&frag, "const");
            assert!(names.contains(&"widgetConfig"), "{lang:?} names: {names:?}");
        }
    }

    #[test]
    fn const_declared_inside_a_function_body_is_not_a_def_node() {
        let src = "function run() {\n    const local_thing = 1;\n    return local_thing;\n}\n";
        let frag = extract_graph_fragment(
            src,
            SupportLang::TypeScript,
            "a.ts",
            "repo",
            "proj",
            "c",
            "s",
        );
        let names = def_names_of_kind(&frag, "const");
        assert!(
            !names.contains(&"local_thing"),
            "function-local const must not become a def node: {names:?}"
        );
    }

    #[test]
    fn destructuring_const_pattern_is_skipped() {
        let src = "export const { a, b } = obj;\nexport const [c, d] = arr;\nexport const real_one = 1;\n";
        let frag = extract_graph_fragment(
            src,
            SupportLang::TypeScript,
            "a.ts",
            "repo",
            "proj",
            "c",
            "s",
        );
        let names = def_names_of_kind(&frag, "const");
        assert!(
            !names.iter().any(|n| ["a", "b", "c", "d"].contains(n)),
            "destructuring patterns must not yield def nodes: {names:?}"
        );
        assert!(names.contains(&"real_one"), "names: {names:?}");
    }

    #[test]
    fn rust_and_python_do_not_emit_const_def_nodes() {
        let rust_frag = extract_graph_fragment(
            "const MAX: usize = 10;\nfn foo() {}\n",
            SupportLang::Rust,
            "a.rs",
            "repo",
            "proj",
            "c",
            "s",
        );
        assert!(def_names_of_kind(&rust_frag, "const").is_empty());

        let py_frag = extract_graph_fragment(
            "SOME_CONST = 1\n\ndef foo():\n    pass\n",
            SupportLang::Python,
            "a.py",
            "repo",
            "proj",
            "c",
            "s",
        );
        assert!(def_names_of_kind(&py_frag, "const").is_empty());
    }
}
