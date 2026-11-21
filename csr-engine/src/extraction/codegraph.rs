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

use std::collections::{BTreeMap, BTreeSet, HashSet};
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
    /// Lexical containers derived from the same AST root as `nodes`. Span
    /// stamping consumes this field to avoid parsing each committed file a
    /// second time solely for witness symbol qualification.
    pub containers: Vec<ContainerSpan>,
    /// TASK 2 (WCR truth pass X4 tier); scope-qualified as of the X4
    /// adversarial-review truth pass (Finding 1), and CHAIN-qualified as of
    /// Finding 4: `(scope, name)` pairs for every local-binding name
    /// declared in this file — function/closure parameters, catch clause
    /// params, and local (non-top-level) variable declarations/destructuring
    /// targets. `scope` is the FULL scope CHAIN (see `scope_chain`'s doc
    /// comment) of the nearest enclosing NAMED definition (function/method/
    /// top-level const fn) the binding sits inside, or `""` for a binding
    /// outside any named def (module-level block scopes, e.g. inside a
    /// top-level `if`/`try`). See `collect_local_bindings`'s and
    /// `scope_chain`'s doc comments. Without scope, a single flat name-set
    /// per (project, file) let a parameter named `handler` in one function
    /// (or one anonymous closure) classify a sibling
    /// function's unrelated `handler()` call as local — the bug this
    /// qualification fixes. Populated from the SAME parse as `nodes`/`edges`
    /// (`extract_inner` calls the shared `collect_local_bindings_from_root`
    /// on its own already-parsed `root`), never a second, separate parse.
    pub local_bindings: BTreeSet<(String, String)>,
    /// X4 adversarial review, Finding 4 (sibling anonymous-closure
    /// conflation); upgraded to ALL DISTINCT chains per edge by the Codex
    /// round 4 adversarial review, Finding 1: the AST scope CHAIN of EVERY
    /// `calls`/`imports` call/import site — keyed identically to `edges`,
    /// `(src_id, dst_id, kind)` — from the SAME parse as
    /// `nodes`/`edges`/`local_bindings`, never a second one. A chain is the
    /// nearest enclosing NAMED def's name followed by `>anon<idx>` for each
    /// anonymous function-kind ancestor between that named def and the site
    /// (outer to inner), `<idx>` a deterministic within-parse preorder
    /// index over every func-kind node in the file (see
    /// `func_node_preorder_index`/`scope_chain`) — e.g. `"Component"` /
    /// `"Component>anon12"` / `"Component>anon12>anon15"`. `""` for a
    /// module-level site.
    ///
    /// `edges` AGGREGATES every physical call/import site sharing the same
    /// `(src_id, dst_id, kind)` triple into ONE edge (`add_edge`'s own
    /// `and_modify` — only `weight` accumulates, `callee_kind`/`evidence`
    /// keep the first occurrence's values). Before Finding 1, this map kept
    /// only the FIRST such site's chain too (`or_insert_with`) — for an
    /// edge aggregating calls from TWO sibling anonymous closures, that
    /// meant the X4 resolver tier evaluated the whole aggregate against
    /// exactly ONE occurrence's chain, order-dependent on `root.find_all`'s
    /// traversal order: it could falsely witness the aggregate via a
    /// binding that only encloses the OTHER (unrecorded) occurrence, or
    /// fail to witness a binding that correctly encloses every recorded
    /// occurrence. The value here is now the FULL SET of distinct chains
    /// (a `BTreeSet` — deterministic order, natural dedup of repeat
    /// occurrences at the identical chain), and the X4 tier classifies
    /// `local` only when a witness's binding chain is a prefix of EVERY
    /// member (conservative universal quantification — see
    /// `resolver::Pending::call_scope_chains`'s doc comment).
    ///
    /// Witness machinery ONLY for the X4 resolver tier's prefix match (see
    /// `local_bindings`'s doc comment and `nearest_def_node`'s Finding 4
    /// note) — GRAPH src attribution (`edges[..].src_id`, still from
    /// `nearest_def_node` alone) is unaffected. Chains are only ever
    /// compared within a single re-extraction of the SAME file content
    /// (`local_bindings` and this map are recomputed together from the same
    /// fresh parse) — never persist one across different file versions as
    /// if directly comparable.
    pub call_scope_chains: BTreeMap<(String, String, String), BTreeSet<String>>,
    /// Codex round 5 adversarial review (backfill re-point correctness):
    /// `(legacy_src_name, current_src_name, callee, kind)` for EVERY fresh
    /// `calls`/`imports` SITE (not aggregated by edge — a single aggregated
    /// `edges` entry can hide several sites with DIFFERENT legacy
    /// attributions, e.g. one site inside a closure and a sibling one that
    /// isn't). `legacy_src_name` is `legacy_src_attribution`'s frozen
    /// pre-92179d1 rule; `current_src_name` is the SAME site's live
    /// `nearest_def_node` attribution (matching `edges[..].src_id`'s owning
    /// node's `name`). Both are NAMES (`code_nodes.name`), never ids — the
    /// module fallback's name is `file`, same convention `calls_any_src`/
    /// `imports_any_src` already use in `backfill_wcr_witnesses`. `callee` is
    /// the bare target name (no `name:` prefix); `kind` is `"calls"` or
    /// `"imports"`, matching `edges[..].kind`.
    ///
    /// Witness machinery ONLY for `backfill_wcr_witnesses`'s re-point
    /// correspondence check (Codex round 5: replaces the round-4
    /// candidate-COUNT re-point rule, which manufactured certainty from
    /// bare-name survival — see `legacy_src_attribution`'s doc comment) —
    /// never used for GRAPH attribution, and never read by any live-pipeline
    /// fragment consumer (`hooks::post_tool_use`, `import::backfill`,
    /// `extraction::repo_scan` all only ever touch `nodes`/`edges`). Same
    /// "computed unconditionally from the one shared parse, consumed
    /// selectively" convention as `local_bindings`/`call_scope_chains`,
    /// immediately above.
    pub call_attribution_pairs: Vec<(String, String, String, String)>,
    /// Finding 3 (X4 adversarial review): `true` iff the tree-sitter parse
    /// of this file produced NO `ERROR`/`MISSING` node anywhere in the tree
    /// (see `tree_has_error`). A degraded/partial parse can still recover
    /// one or more real def nodes while silently dropping others (e.g. every
    /// module-level import) — without this flag, `backfill_wcr_witnesses`'s
    /// drift gate would credit that partial recovery as if the file were
    /// genuinely edited, wrongly drift-classifying (and thus hiding) every
    /// vanished module-level edge. `false` on every `GraphFragment::default()`
    /// fallback (unsupported language, undersized source, or a `catch_unwind`
    /// panic) — the safe default, since those paths already produce zero def
    /// nodes and must never be trusted as drift evidence either.
    pub parse_clean: bool,
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
/// `pub(crate)` (WCR truth pass, Codex round 6): `eval::codegraph`'s tests
/// need to compute the SAME hash a fresh re-parse would produce, to seed
/// fixtures whose stored `code_nodes.body_hash` deterministically matches
/// (or deliberately mismatches) current on-disk content — see
/// `historical_src_content_unchanged`'s doc comment.
pub(crate) fn body_hash(text: &str) -> String {
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

/// TASK 1 (WCR truth pass, "await-glue" bug): TS/JS's grammar disambiguates a
/// generic call immediately after `await` — `await foo<T>(...)`, `await
/// obj.m<T>(...)` — by attaching the `call_expression`'s `function` field to
/// an `await_expression` node whose OWN text is `"await foo"` / `"await
/// obj.m"` (the literal `await` keyword glued to the real callee expression
/// by the parser, with `type_arguments` and `arguments` as trailing SIBLINGS
/// of that `await_expression`, not children of a nested call) — verified by
/// dumping the tree-sitter-typescript parse of `await metaFetch<any[]>(...)`
/// (a real, live corpus example, `anukriti-command-center/src/lib/data/meta.ts`).
/// Every downstream callee-text helper (`is_method_callee_kind`,
/// `call_qualifier`, `bare_callee`) assumes its `func` argument names the
/// callee directly; fed the outer `await_expression` node as-is, `bare_callee`
/// happily strips the space between "await" and the real name, producing a
/// garbage callee like `awaitmetaFetch` — no def anywhere can ever match it,
/// permanently swelling the unexplained residual.
///
/// Descends past the literal `await` keyword token to the `await_expression`'s
/// one non-keyword child — the real callee expression (an `identifier` for
/// `await foo<T>()`, a `member_expression` for `await obj.m<T>()`) — so every
/// caller downstream operates on the correct node. A plain (non-generic)
/// `await foo(...)` never hits this at all: its `call_expression`'s own
/// `function` field is already the bare `identifier`/`member_expression`
/// directly (verified the same way), with the `await_expression` sitting
/// ABOVE the `call_expression` as its parent, never as its `function` field —
/// this helper's `kind() != "await_expression"` guard is a true no-op for
/// that (overwhelmingly common) shape. Returns the original `func` unchanged
/// on any other kind, or on the (believed-unreachable, but never-guess)
/// shape where an `await_expression` has no non-`await` child at all.
fn unwrap_await_glued_function<D: ast_grep_core::Doc>(
    func: ast_grep_core::Node<'_, D>,
) -> ast_grep_core::Node<'_, D> {
    if func.kind().as_ref() != "await_expression" {
        return func;
    }
    let inner = func.children().find(|c| c.kind().as_ref() != "await");
    inner.unwrap_or(func)
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

// ─── TASK 2 (WCR truth pass): local-binding name collection, X4 tier ───
//
// The X4 `local` classify tier (`resolver::resolve_edges`) needs real,
// disk-verifiable proof that a bare name IS bound SOMEWHERE in a specific
// file before it will classify an otherwise-unexplained edge as a local
// scope reference — never a guess from name shape alone. This is that
// proof: the set of names bound as function/closure parameters (including
// destructured/defaulted/rest forms and catch-clause params) and local
// (non-top-level) variable declarations, gathered from real tree-sitter AST
// structure, one recursive pattern-walker per language family. Every AST
// shape below was verified against a real parse before being coded (never
// guessed from grammar docs alone) — unrecognized/unverified node kinds are
// silently skipped (a false negative, never a false positive: `match`'s
// `_ => {}` arm never fabricates a name).

/// The set of `(scope, name)` local-binding pairs declared in `source` — see
/// this section's module-doc-comment-style header above, and `scope_chain`
/// for what `scope` means (as of the X4 adversarial review, Finding 4: a
/// full scope CHAIN, not a bare enclosing-def name). Independently parses
/// `source` (unlike `collect_local_bindings_from_root`, which reuses an
/// already-parsed root — see that function's doc comment for the
/// single-parse production path via `extract_inner`/`GraphFragment`); this
/// entry point exists for standalone unit-testing per language and for any
/// future caller that doesn't already have a parsed root at hand. Wrapped in
/// `catch_unwind` (malformed input can panic mid-parse), same convention as
/// `extract_graph_fragment`.
pub fn collect_local_bindings(source: &str, lang: SupportLang) -> BTreeSet<(String, String)> {
    if source.len() < 2 {
        return BTreeSet::new();
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        let grep = lang.ast_grep(source);
        let root = grep.root();
        let func_index = func_node_preorder_index(&root, lang);
        collect_local_bindings_from_root(&root, lang, &func_index)
    }));
    result.unwrap_or_default()
}

/// Shared by `collect_local_bindings` (standalone parse, for unit tests) and
/// `extract_inner` (reuses its own already-parsed `root` AND `func_index` —
/// no second parse, no second index pass; see `scope_chain`'s doc comment on
/// why bindings and call/import-site chains must share ONE `func_index`).
fn collect_local_bindings_from_root<D: ast_grep_core::Doc>(
    root: &ast_grep_core::Node<'_, D>,
    lang: SupportLang,
    func_index: &BTreeMap<usize, usize>,
) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    match lang {
        SupportLang::TypeScript | SupportLang::Tsx | SupportLang::JavaScript => {
            collect_ts_js_local_bindings(root, lang, func_index, &mut out);
        }
        SupportLang::Python => {
            collect_python_local_bindings(root, lang, func_index, &mut out);
        }
        SupportLang::Rust => {
            collect_rust_local_bindings(root, lang, func_index, &mut out);
        }
        _ => {}
    }
    out
}

/// WCR truth-pass X4 remediation (real-world TSX repro session): the nearest
/// enclosing DEFINITION ancestor of `node` — a NAMED function/method (any
/// `func_kinds` ancestor with a `child_name`) or a top-level const-assigned
/// function expression (`top_level_const_fn_name`) — walking PAST anonymous
/// function-kind ancestors (closures, or a non-top-level `const helper = ()
/// => {}` nested inside another function) to find it. Returns
/// `(canonical_kind, name)`: `canonical_kind(anc.kind(), lang)` (always
/// `"function"` today — see that function) for a real `func_kinds` match, or
/// the literal `"const"` kind — matching the def-node kind `extract_inner`'s
/// top-level-const loop creates — for a top-level const-assigned function
/// expression. `None` when `node` is not inside any such definition
/// (module-level code, including inside a top-level `if`/`try` block).
///
/// Single source of truth for "which def node does this AST position belong
/// to", shared by BOTH `scope_chain` (X4 local-binding/call-site scope
/// tagging, below) and `extract_inner`'s own `calls`-edge source-symbol
/// walk. Before this fix the two had subtly different rules: the edges walk
/// broke at the FIRST function-kind ancestor even when it was anonymous,
/// silently falling back to the MODULE node instead of continuing outward —
/// so a `playTrack()` call sitting inside a sibling `useCallback` closure
/// and the `const playTrack = useCallback(...)` binding it targets ended up
/// disagreeing on scope (`""`/module vs the enclosing component's name) even
/// though both plainly belong to the same component, and the X4 tier's
/// scope-equality check (`resolver::classify_only`) could never match them.
/// Confirmed against real-world TSX (`anukriti-mvp-expo/src/context/
/// radio-context.tsx`, `app/radio-player.tsx`): every call from inside a
/// `useCallback`/`useEffect`/`useFocusEffect` closure — i.e. nearly every
/// call site in idiomatic React hook code — was mis-attributed to module
/// scope this way, which is why `had_local_binding` was `false` for the
/// entire WCR unexplained residual despite obvious component-local names.
fn nearest_def_node<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
    lang: SupportLang,
) -> Option<(&'static str, String)> {
    for anc in node.ancestors() {
        if !func_kinds(lang).contains(&anc.kind().as_ref()) {
            continue;
        }
        if let Some(name) = child_name(&anc) {
            return Some((canonical_kind(anc.kind().as_ref(), lang), name));
        }
        if let Some(name) = top_level_const_fn_name(&anc) {
            return Some(("const", name));
        }
        // Anonymous, non-top-level-const function-kind ancestor (e.g. a bare
        // closure, or a `const helper = () => {}` nested inside another
        // function): not a definition itself — keep walking up for an outer
        // one rather than stopping here.
    }
    None
}

/// WCR truth-pass Codex round 5 finding: a FROZEN reimplementation of the
/// PRE-commit-92179d1 calls-edge src attribution rule — kept SOLELY as a
/// migration witness for `backfill_wcr_witnesses`'s re-point correspondence
/// check (`GraphFragment::call_attribution_pairs`), never for live GRAPH
/// attribution (`nearest_def_node` above is the current, correct rule for
/// that — this function must never replace it anywhere else).
///
/// The OLD walk, verbatim (see `extract_inner`'s pre-92179d1 calls-edge loop
/// in git history): break at the FIRST `func_kinds` ancestor, unconditionally
/// — never walking past it, unlike `nearest_def_node`. If that ancestor has
/// a name (`child_name`), attribute to it; otherwise (an ANONYMOUS
/// function-kind ancestor — a closure, or even a top-level const-assigned
/// arrow function, since the old walk never called
/// `top_level_const_fn_name` either) attribute to the MODULE — `None` here,
/// matching `nearest_def_node`'s own `None` convention (caller falls back to
/// `module_id`/the module's `name` field, which is `file`). If `node` has NO
/// `func_kinds` ancestor at all, also `None` (module) — same as the old
/// walk's initial `src = module_id` never being overwritten.
///
/// Why this matters: historical `code_edges` rows extracted before
/// 92179d1 carry THIS rule's attribution, not `nearest_def_node`'s. A
/// historical edge whose src no longer matches the fresh extraction's
/// CURRENT attribution for that exact call site is not necessarily drift —
/// it may simply be the SAME physical call, attributed under the OLD rule,
/// now attributed differently under the NEW one. This function lets the
/// backfill prove that correspondence mechanically (same call site, same
/// callee) rather than guessing from bare-name candidate-count alone (Codex
/// round 5: candidate-count uniqueness is not identity — a genuinely
/// REMOVED call in one function plus one unrelated surviving caller of the
/// same bare name is indistinguishable from attribution skew by count
/// alone, and was being wrongly re-pointed to the unrelated survivor).
fn legacy_src_attribution<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
    lang: SupportLang,
) -> Option<(&'static str, String)> {
    for anc in node.ancestors() {
        if !func_kinds(lang).contains(&anc.kind().as_ref()) {
            continue;
        }
        if let Some(name) = child_name(&anc) {
            return Some((canonical_kind(anc.kind().as_ref(), lang), name));
        }
        // Anonymous first func-kind ancestor: the old walk broke here
        // WITHOUT updating `src` — falls straight to module, never walking
        // outward for a named ancestor the way `nearest_def_node` does.
        return None;
    }
    None
}

/// Assigns a stable preorder index to EVERY `func_kinds` node in this parse
/// (named or anonymous alike) — the deterministic within-parse identity
/// `scope_chain` uses to name an otherwise-nameless anonymous closure
/// (`anon<idx>`). Built once per fragment/parse (`extract_inner` builds one
/// from its own `root` and shares it across the calls-edge loop AND the
/// local-binding collectors — never a second, separately-indexed pass) via
/// `root.dfs()` — `ast_grep_core`'s preorder traversal (visits a node
/// before its children), so sibling closures at the same nesting depth are
/// indexed in left-to-right source order, deterministically. Keyed by
/// `Node::node_id()` (tree-sitter's own per-node identity, stable for the
/// lifetime of THIS parse tree — see that method's doc comment) rather than
/// a byte range, though either would work here.
fn func_node_preorder_index<D: ast_grep_core::Doc>(
    root: &ast_grep_core::Node<'_, D>,
    lang: SupportLang,
) -> BTreeMap<usize, usize> {
    let mut out = BTreeMap::new();
    let mut idx = 0usize;
    for n in root.dfs() {
        if func_kinds(lang).contains(&n.kind().as_ref()) {
            out.insert(n.node_id(), idx);
            idx += 1;
        }
    }
    out
}

/// X4 adversarial review, Finding 4 (sibling anonymous-closure conflation):
/// the full scope CHAIN for `node` — the nearest enclosing NAMED def's name
/// (via the SAME walk as `nearest_def_node`), followed by `>anon<idx>` for
/// each anonymous function-kind ancestor encountered strictly BETWEEN that
/// named def and `node` (outer to inner), `<idx>` from `func_index` (see
/// `func_node_preorder_index`). `""` when `node` is not inside any named def
/// (module-level code) — same "no named def" case `nearest_def_node`
/// returns `None` for.
///
/// `nearest_def_node` (GRAPH src attribution, and the bare-name X4 scope
/// this chain replaces) deliberately flattens every anonymous closure onto
/// the outer named def — correct for graph attribution (many closures
/// really do belong to one component/function), but it means a binding
/// declared in anonymous closure A and a call in an unrelated SIBLING
/// closure B both report the SAME bare scope name, so a flat scope-equality
/// match (the X4 tier before this fix) cannot tell them apart — see
/// `ResolveStats::local`'s doc comment and `classify_only`'s X4 block. This
/// chain is witness machinery ONLY: `nearest_def_node` itself, and the
/// GRAPH `calls`-edge src attribution built from it, are UNCHANGED.
///
/// Chains are only ever compared within a single `backfill_wcr_witnesses`
/// re-extraction of the SAME file content — bindings and pending-edge
/// chains are recomputed together from the SAME fresh parse — so a
/// within-parse `func_index` is stable enough for that purpose. Never
/// persist a chain across different file versions (a different parse) as
/// if directly comparable — the `anon<idx>` values have no meaning outside
/// the parse that produced them.
fn scope_chain<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
    lang: SupportLang,
    func_index: &BTreeMap<usize, usize>,
) -> String {
    let mut anon_indices: Vec<usize> = Vec::new();
    for anc in node.ancestors() {
        if !func_kinds(lang).contains(&anc.kind().as_ref()) {
            continue;
        }
        if let Some(name) = child_name(&anc) {
            return assemble_chain(name, anon_indices);
        }
        if let Some(name) = top_level_const_fn_name(&anc) {
            return assemble_chain(name, anon_indices);
        }
        // Anonymous, non-top-level-const function-kind ancestor: record its
        // within-parse index (innermost-first, reversed by `assemble_chain`
        // into outer-to-inner reading order) and keep walking up for the
        // named def, exactly like `nearest_def_node`.
        if let Some(&idx) = func_index.get(&anc.node_id()) {
            anon_indices.push(idx);
        }
    }
    String::new()
}

/// Outer-to-inner chain string: `name` followed by `>anon<idx>` for each
/// entry in `anon_indices` (which arrives innermost-first from the
/// ancestor walk — reversed here before assembly).
fn assemble_chain(name: String, mut anon_indices: Vec<usize>) -> String {
    anon_indices.reverse();
    let mut chain = name;
    for idx in anon_indices {
        chain.push_str(&format!(">anon{idx}"));
    }
    chain
}

/// X4 adversarial review, Finding 1: `Some(name)` when `anc` (a function-kind
/// AST node with no identifier child of its own — i.e. `child_name` already
/// returned `None`) is the `value` of a top-level `const`/`let`/`var`
/// declarator, e.g. `const loadTrack = () => {...}` at module scope — the
/// def-node infrastructure (`const_decl_kinds`/`is_program_level_declaration`/
/// `const_decl_names`) already treats this shape as a real, named, repo-wide
/// def, so the local-binding scope tagger must name it identically (`"loadTrack"`)
/// for the resolver's scope-equality match to have anything to match against.
/// A NON-top-level `const helper = () => {}` (nested inside another function)
/// deliberately returns `None` here — it is not a def node, so it must not be
/// treated as a named scope either; `scope_chain`/`nearest_def_node` keep
/// walking upward past it instead, attributing to the enclosing named
/// function (recording an `>anon<idx>` chain segment for it, in
/// `scope_chain`'s case).
fn top_level_const_fn_name<D: ast_grep_core::Doc>(
    anc: &ast_grep_core::Node<'_, D>,
) -> Option<String> {
    let declarator = anc.parent()?;
    if declarator.kind().as_ref() != "variable_declarator" {
        return None;
    }
    let declaration = declarator.parent()?;
    if !is_program_level_declaration(&declaration) {
        return None;
    }
    let name_node = declarator.field("name")?;
    if name_node.kind().as_ref() == "identifier" {
        Some(name_node.text().to_string())
    } else {
        None
    }
}

fn insert_binding_name(out: &mut BTreeSet<(String, String)>, scope: &str, name: impl Into<String>) {
    let name = name.into();
    // Same len>=2 floor as `is_noise_callee`: a single-char name can never
    // be the target of a pending `calls`/`imports` placeholder edge in the
    // first place (extraction drops single-char callees/import symbols), so
    // a single-char local-binding entry could never be looked up by the X4
    // tier — skip recording it at all.
    if name.len() >= 2 {
        out.insert((scope.to_string(), name));
    }
}

/// TS/TSX/JS local bindings: every parameter list in the file (function
/// declarations, arrow functions, method definitions, function expressions
/// all share the `formal_parameters` node kind), every `catch_clause`'s
/// `parameter` field, and local (non-top-level — see
/// `is_program_level_declaration`) `const`/`let`/`var` declarations,
/// INCLUDING destructuring targets (unlike `const_decl_names`, which backs
/// TOP-LEVEL def nodes and deliberately skips destructuring — there is no
/// ambiguity to avoid here, a local binding is never a repo-wide symbol).
fn collect_ts_js_local_bindings<D: ast_grep_core::Doc>(
    root: &ast_grep_core::Node<'_, D>,
    lang: SupportLang,
    func_index: &BTreeMap<usize, usize>,
    out: &mut BTreeSet<(String, String)>,
) {
    let params_matcher = KindMatcher::new("formal_parameters", lang);
    for params in root.find_all(&params_matcher) {
        // `params` is always a direct child of the function/method/arrow
        // node it belongs to, so the scope chain FROM `params` itself is
        // exactly that function's own chain (or the outer named def's chain
        // plus this closure's own `>anon<idx>` segment, if it's anonymous)
        // — computed once per parameter list, not per name.
        let scope = scope_chain(&params, lang, func_index);
        for child in params.children() {
            if child.is_named() {
                ts_js_pattern_names(&child, &scope, out);
            }
        }
    }
    let catch_matcher = KindMatcher::new("catch_clause", lang);
    for clause in root.find_all(&catch_matcher) {
        let scope = scope_chain(&clause, lang, func_index);
        if let Some(param) = clause.field("parameter") {
            ts_js_pattern_names(&param, &scope, out);
        }
    }
    for kind in const_decl_kinds(lang) {
        let matcher = KindMatcher::new(kind, lang);
        for decl in root.find_all(&matcher) {
            if is_program_level_declaration(&decl) {
                // Already a `const`-kind def node (WCR Phase 7, TASK C) —
                // recording it again here would double-count a repo-wide
                // symbol as a "local" one.
                continue;
            }
            let scope = scope_chain(&decl, lang, func_index);
            for child in decl.children() {
                if child.is_named() && child.kind().as_ref() == "variable_declarator" {
                    if let Some(name) = child.field("name") {
                        ts_js_pattern_names(&name, &scope, out);
                    }
                }
            }
        }
    }
}

/// Recursive TS/JS binding-pattern name extractor — every AST shape here was
/// verified against a real parse (see this section's header comment).
/// Handles: bare identifiers, object/array destructuring (including nested,
/// renamed (`{a: b}`), defaulted (`{a = 1}`/`a = 1`), and rest (`...rest`)
/// forms), and the TS `required_parameter`/`optional_parameter` wrapper
/// (whose `pattern` field holds the actual binding, `type`/`value` fields
/// holding the type annotation / default value — neither a binding name).
/// `scope` (X4 adversarial review, Finding 1) is the enclosing named def's
/// name, precomputed once by the caller for the whole binding SITE (a
/// parameter list, catch clause, or declaration) — every name found while
/// recursing through one site shares that site's scope.
fn ts_js_pattern_names<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
    scope: &str,
    out: &mut BTreeSet<(String, String)>,
) {
    match node.kind().as_ref() {
        "identifier" | "shorthand_property_identifier_pattern" => {
            insert_binding_name(out, scope, node.text().to_string());
        }
        "object_pattern" | "array_pattern" => {
            for child in node.children() {
                if child.is_named() {
                    ts_js_pattern_names(&child, scope, out);
                }
            }
        }
        "pair_pattern" => {
            // `{a: renamed}` — `key` is the source property name (not a
            // binding), `value` is the actual bound local name.
            if let Some(value) = node.field("value") {
                ts_js_pattern_names(&value, scope, out);
            }
        }
        "rest_pattern" => {
            for child in node.children() {
                if child.is_named() {
                    ts_js_pattern_names(&child, scope, out);
                }
            }
        }
        "assignment_pattern" | "object_assignment_pattern" => {
            // `x = 1` / `{a = 1}` — `left` is the bound name, `right` the
            // default-value expression (not a binding).
            if let Some(left) = node.field("left") {
                ts_js_pattern_names(&left, scope, out);
            }
        }
        "required_parameter" | "optional_parameter" => {
            if let Some(pattern) = node.field("pattern") {
                ts_js_pattern_names(&pattern, scope, out);
            }
        }
        _ => {}
    }
}

/// Python local bindings, per this tier's spec: function parameters (every
/// `parameters` node) and assignment targets INSIDE function bodies only
/// (module-level Python assignments are out of scope for this tier — see
/// `is_inside_python_function`).
fn collect_python_local_bindings<D: ast_grep_core::Doc>(
    root: &ast_grep_core::Node<'_, D>,
    lang: SupportLang,
    func_index: &BTreeMap<usize, usize>,
    out: &mut BTreeSet<(String, String)>,
) {
    let params_matcher = KindMatcher::new("parameters", lang);
    for params in root.find_all(&params_matcher) {
        let scope = scope_chain(&params, lang, func_index);
        for child in params.children() {
            if child.is_named() {
                python_pattern_names(&child, &scope, out);
            }
        }
    }
    let assign_matcher = KindMatcher::new("assignment", lang);
    for assign in root.find_all(&assign_matcher) {
        if !is_inside_python_function(&assign) {
            continue;
        }
        let scope = scope_chain(&assign, lang, func_index);
        if let Some(left) = assign.field("left") {
            python_pattern_names(&left, &scope, out);
        }
    }
}

/// True when `node` has a `function_definition` ancestor.
fn is_inside_python_function<D: ast_grep_core::Doc>(node: &ast_grep_core::Node<'_, D>) -> bool {
    node.ancestors()
        .any(|anc| anc.kind().as_ref() == "function_definition")
}

/// Recursive Python binding-pattern name extractor — every AST shape here
/// was verified against a real parse (see this section's header comment).
/// Handles: bare identifiers, `typed_parameter` (`a: int` — no `name` field;
/// the identifier is the first named child before the `:`), `default_parameter`/
/// `typed_default_parameter` (`name` field is the bound identifier, `value`
/// the default expression), `*args`/`**kwargs` splat patterns, and bare tuple
/// unpacking targets (`pattern_list`, e.g. `a, b = 1, 2`).
fn python_pattern_names<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
    scope: &str,
    out: &mut BTreeSet<(String, String)>,
) {
    match node.kind().as_ref() {
        "identifier" => {
            insert_binding_name(out, scope, node.text().to_string());
        }
        "typed_parameter" => {
            if let Some(name) = node
                .children()
                .find(|c| c.is_named() && c.kind().as_ref() == "identifier")
            {
                python_pattern_names(&name, scope, out);
            }
        }
        "default_parameter" | "typed_default_parameter" => {
            if let Some(name) = node.field("name") {
                python_pattern_names(&name, scope, out);
            }
        }
        "list_splat_pattern" | "dictionary_splat_pattern" | "pattern_list" => {
            for child in node.children() {
                if child.is_named() {
                    python_pattern_names(&child, scope, out);
                }
            }
        }
        _ => {}
    }
}

/// Rust local bindings, per this tier's spec: `let` bindings (any depth,
/// including `if let`/`while let` condition patterns and destructuring) and
/// closure parameters. Deliberately NOT regular `fn` parameters — out of
/// this tier's spec.
fn collect_rust_local_bindings<D: ast_grep_core::Doc>(
    root: &ast_grep_core::Node<'_, D>,
    lang: SupportLang,
    func_index: &BTreeMap<usize, usize>,
    out: &mut BTreeSet<(String, String)>,
) {
    for kind in ["let_declaration", "let_condition"] {
        let matcher = KindMatcher::new(kind, lang);
        for decl in root.find_all(&matcher) {
            let scope = scope_chain(&decl, lang, func_index);
            if let Some(pattern) = decl.field("pattern") {
                rust_pattern_names(&pattern, &scope, out);
            }
        }
    }
    let closure_matcher = KindMatcher::new("closure_parameters", lang);
    for params in root.find_all(&closure_matcher) {
        // Closures are not `func_kinds` themselves (Rust's `func_kinds` is
        // only `function_item`), so `scope_chain` walks straight past the
        // closure to the enclosing named function with no `>anon<idx>`
        // segment at all — exactly the "nested closures attribute to F"
        // rule, unchanged from before chains existed.
        let scope = scope_chain(&params, lang, func_index);
        for child in params.children() {
            if !child.is_named() {
                continue;
            }
            if child.kind().as_ref() == "parameter" {
                if let Some(pattern) = child.field("pattern") {
                    rust_pattern_names(&pattern, &scope, out);
                }
            } else {
                rust_pattern_names(&child, &scope, out);
            }
        }
    }
}

/// Recursive Rust binding-pattern name extractor — every AST shape here was
/// verified against a real parse (see this section's header comment).
/// Handles: bare identifiers, `mut` bindings (`mut_pattern` wraps the
/// identifier alongside a `mutable_specifier` token — the `let_declaration`/
/// `parameter` `pattern` field itself is already the bare identifier for a
/// top-level `let mut x`, so `mut_pattern` is only reached nested inside a
/// tuple/struct pattern, e.g. `let (mut a, b) = ...`), tuple destructuring
/// (`tuple_pattern`), enum/tuple-struct destructuring (`tuple_struct_pattern`
/// — its `type` field is the variant/type path, e.g. `Some`, NEVER a
/// binding; every OTHER named child is), and struct destructuring
/// (`struct_pattern` -> `field_pattern` children, each either a renamed
/// binding (`pattern` field holds the real local name, `name` field is the
/// STRUCT FIELD name, not a binding) or a shorthand binding (`name` field's
/// own `shorthand_field_identifier` kind IS the binding)).
fn rust_pattern_names<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
    scope: &str,
    out: &mut BTreeSet<(String, String)>,
) {
    match node.kind().as_ref() {
        "identifier" | "shorthand_field_identifier" => {
            insert_binding_name(out, scope, node.text().to_string());
        }
        "tuple_pattern" | "mut_pattern" => {
            for child in node.children() {
                if child.is_named() {
                    rust_pattern_names(&child, scope, out);
                }
            }
        }
        "tuple_struct_pattern" => {
            let type_range = node.field("type").map(|t| t.range());
            for child in node.children() {
                if !child.is_named() {
                    continue;
                }
                if Some(child.range()) == type_range {
                    continue;
                }
                rust_pattern_names(&child, scope, out);
            }
        }
        "struct_pattern" => {
            for child in node.children() {
                if child.is_named() && child.kind().as_ref() == "field_pattern" {
                    rust_pattern_names(&child, scope, out);
                }
            }
        }
        "field_pattern" => {
            if let Some(pattern) = node.field("pattern") {
                rust_pattern_names(&pattern, scope, out);
            } else if let Some(name) = node.field("name") {
                if name.kind().as_ref() == "shorthand_field_identifier" {
                    rust_pattern_names(&name, scope, out);
                }
            }
        }
        _ => {}
    }
}

/// Finding 3 (X4 adversarial review): `true` iff `root` (and, transitively,
/// every descendant) is free of `ERROR`/`MISSING` tree-sitter nodes — a
/// genuinely clean parse, not a degraded/partial recovery. `ast_grep_core`'s
/// `Node` does not expose a single aggregate "has any error in this subtree"
/// call the way raw `tree_sitter::Node::has_error` does; the equivalent here
/// is a full `dfs()` walk checking each node's own `is_error()`/`is_missing()`
/// (an `ERROR`-kind node for genuinely unparseable text, a zero-width
/// `MISSING` node the parser synthesized to recover from a truncated/invalid
/// construct — tree-sitter's two distinct error-recovery signals, both
/// checked). `dfs()` iterates ALL children (named and unnamed), so nothing is
/// skipped. Used to gate `backfill_wcr_witnesses`'s drift classification: a
/// partial extraction that still recovers one real def node must not be
/// trusted as "this file was genuinely edited" when the parse tree it came
/// from also contains error/missing nodes elsewhere.
fn tree_has_error<D: ast_grep_core::Doc>(root: &ast_grep_core::Node<'_, D>) -> bool {
    root.dfs().any(|n| n.is_error() || n.is_missing())
}

/// One lexical container span, computed SOLELY to qualify function/method
/// symbol identity in the `witness_ledger` minting path (see
/// `import::backfill::stamp_spans_into`/`stamp_spans_historical_into`) —
/// deliberately NEVER written to `NodeRow`/`code_nodes`. Extraction identity
/// there stays exactly `(file, kind, name)` (see `node_id`'s doc comment);
/// this is a witness-ledger-only disambiguation layer added after an
/// adversarial-gate audit found that identity collides two unrelated
/// same-named methods in different `impl`/class bodies into one cross-commit
/// witness join (e.g. `CodeContext::is_empty` and `AstDiff::is_empty`, both
/// bare `is_empty` under the old rule).
///
/// Rust `impl Type { .. }` / `impl Trait for Type { .. }` containers use
/// `::`; TS/JS/TSX classes, named functions, object-literal
/// property paths, and const-bound arrows use `.`; Python classes and named
/// functions use `.`. Container names are themselves ancestor-qualified, so
/// deeper definitions retain their complete lexical path.
#[derive(Debug, Clone)]
pub struct ContainerSpan {
    pub name: String,
    pub separator: &'static str,
    pub start: i64,
    pub end: i64,
}

/// Lexical container spans in `source`, for function/method qualification
/// only (see `ContainerSpan`'s doc comment). Empty for languages with no
/// supported deterministic container walk (Go method receivers are a
/// materially different shape and remain deliberately out of scope) or when
/// the source is too small to parse meaningfully.
/// Wrapped in `catch_unwind` for the same reason `extract_graph_fragment`
/// is: malformed input can panic inside tree-sitter/ast-grep.
pub fn container_spans(source: &str, lang: SupportLang) -> Vec<ContainerSpan> {
    if source.len() < 4 {
        return Vec::new();
    }
    catch_unwind(AssertUnwindSafe(|| container_spans_inner(source, lang))).unwrap_or_default()
}

fn container_spans_inner(source: &str, lang: SupportLang) -> Vec<ContainerSpan> {
    let grep = lang.ast_grep(source);
    let root = grep.root();
    container_spans_from_root(&root, lang)
}

fn container_spans_from_root<D: ast_grep_core::Doc>(
    root: &ast_grep_core::Node<'_, D>,
    lang: SupportLang,
) -> Vec<ContainerSpan> {
    let separator = match lang {
        SupportLang::Rust => "::",
        SupportLang::Python
        | SupportLang::TypeScript
        | SupportLang::Tsx
        | SupportLang::JavaScript => ".",
        _ => return Vec::new(),
    };
    let mut out = Vec::new();

    let named_kinds: &[&str] = match lang {
        SupportLang::Rust => &["impl_item"],
        SupportLang::Python => &["class_definition", "function_definition"],
        SupportLang::TypeScript | SupportLang::Tsx | SupportLang::JavaScript => &[
            "class_declaration",
            "function_declaration",
            "method_definition",
        ],
        _ => unreachable!("unsupported languages returned above"),
    };
    for kind in named_kinds {
        let matcher = KindMatcher::new(kind, lang);
        for n in root.find_all(&matcher) {
            if let Some(name) = container_name(&n, lang) {
                push_container(&mut out, &n, name, separator);
            }
        }
    }

    if matches!(
        lang,
        SupportLang::TypeScript | SupportLang::Tsx | SupportLang::JavaScript
    ) {
        let object_matcher = KindMatcher::new("object", lang);
        for n in root.find_all(&object_matcher) {
            if let Some(name) = direct_binding_or_property_name(&n) {
                push_container(&mut out, &n, name, separator);
            }
        }

        let arrow_matcher = KindMatcher::new("arrow_function", lang);
        for n in root.find_all(&arrow_matcher) {
            if let Some(name) = ancestor_binding_or_property_name(&n) {
                push_container(&mut out, &n, name, separator);
            }
        }
    }

    qualify_container_paths(out)
}

fn push_container<D: ast_grep_core::Doc>(
    out: &mut Vec<ContainerSpan>,
    node: &ast_grep_core::NodeMatch<'_, D>,
    name: String,
    separator: &'static str,
) {
    // Same min-length guard `extract_inner`'s def loop applies: never useful
    // provenance, only orphan cruft from a misparsed fragment.
    if name.len() < 2 {
        return;
    }
    let text = node.text().to_string();
    let start = node.start_pos().line() as i64;
    let end = start + text.lines().count().saturating_sub(1) as i64;
    out.push(ContainerSpan {
        name,
        separator,
        start,
        end,
    });
}

fn direct_binding_or_property_name<D: ast_grep_core::Doc>(
    node: &ast_grep_core::NodeMatch<'_, D>,
) -> Option<String> {
    let parent = node.parent()?;
    binding_or_property_name(&parent)
}

fn ancestor_binding_or_property_name<D: ast_grep_core::Doc>(
    node: &ast_grep_core::NodeMatch<'_, D>,
) -> Option<String> {
    for ancestor in node.ancestors() {
        if let Some(name) = binding_or_property_name(&ancestor) {
            return Some(name);
        }
    }
    None
}

fn binding_or_property_name<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
) -> Option<String> {
    let field = match node.kind().as_ref() {
        "variable_declarator" => node.field("name"),
        "pair" => node.field("key"),
        "assignment_expression" => node.field("left"),
        _ => None,
    }?;
    match field.kind().as_ref() {
        "identifier" | "property_identifier" | "shorthand_property_identifier" => {
            Some(field.text().to_string())
        }
        "string" | "string_fragment" => {
            let text = field.text();
            let trimmed = text.trim_matches(['\'', '"']);
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        _ => None,
    }
}

fn qualify_container_paths(mut containers: Vec<ContainerSpan>) -> Vec<ContainerSpan> {
    containers
        .sort_by(|a, b| (a.start, a.end, a.name.as_str()).cmp(&(b.start, b.end, b.name.as_str())));
    containers.dedup_by(|a, b| {
        a.start == b.start && a.end == b.end && a.name == b.name && a.separator == b.separator
    });

    let raw_names: Vec<String> = containers.iter().map(|c| c.name.clone()).collect();
    let mut qualified: Vec<Option<String>> = vec![None; containers.len()];
    for index in 0..containers.len() {
        let name = qualified_container_name(index, &containers, &raw_names, &mut qualified);
        containers[index].name = name;
    }
    containers
}

fn qualified_container_name(
    index: usize,
    containers: &[ContainerSpan],
    raw_names: &[String],
    qualified: &mut [Option<String>],
) -> String {
    if let Some(name) = &qualified[index] {
        return name.clone();
    }
    let current = &containers[index];
    let parent = containers
        .iter()
        .enumerate()
        .filter(|(candidate_index, candidate)| {
            *candidate_index != index
                && candidate.start <= current.start
                && current.end <= candidate.end
                && (candidate.start < current.start || current.end < candidate.end)
        })
        .min_by_key(|(_, candidate)| candidate.end - candidate.start)
        .map(|(candidate_index, _)| candidate_index);

    let name = match parent {
        Some(parent_index) => format!(
            "{}{}{}",
            qualified_container_name(parent_index, containers, raw_names, qualified),
            containers[parent_index].separator,
            raw_names[index]
        ),
        None => raw_names[index].clone(),
    };
    qualified[index] = Some(name.clone());
    name
}

/// A container node's own name. Rust `impl_item` needs dedicated handling
/// (never `extract_name_from_def`'s generic first-identifier-child scan):
/// for `impl Trait for Type { .. }`, that scan's first identifier-like
/// child is `Trait` (the `trait` field, which appears before `for` in the
/// grammar), not `Type` — precisely the receiver/trait confusion this
/// module exists to avoid. The `type` field (present on every `impl_item`,
/// trait impl or not) is always the type actually being implemented, so it
/// alone gives the right name; TS/JS/Python container names are ordinary
/// class-name identifiers, handled by the same shared helper every other
/// def node in this module uses.
fn container_name<D: ast_grep_core::Doc>(
    node: &ast_grep_core::NodeMatch<'_, D>,
    lang: SupportLang,
) -> Option<String> {
    if lang == SupportLang::Rust {
        let field = node.field("type")?;
        return first_type_identifier(&field);
    }
    extract_name_from_def(node, lang)
}

/// Base type identifier under a Rust `impl` block's `type` field,
/// recursing past generic/reference wrappers: `impl<T> Foo<T>` and
/// `impl<'a> &'a Foo` both resolve to `Foo`, matching how the type is
/// referred to at call sites (`Foo::method(..)`), never `Foo<T>::method` or
/// `&Foo::method`. First DFS-order `type_identifier` wins — a bounded walk
/// over one type expression's subtree, not the whole file.
fn first_type_identifier<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
) -> Option<String> {
    if node.kind().as_ref() == "type_identifier" {
        return Some(node.text().to_string());
    }
    for child in node.children() {
        if let Some(name) = first_type_identifier(&child) {
            return Some(name);
        }
    }
    None
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
    // Per-parse occurrence identity MUST retain span. `NodeRow::id` is the
    // intentionally coarse persisted graph identity `(repo,file,kind,name)`;
    // using it as this collection key used to overwrite coexisting methods
    // such as `CodeContext::is_empty` and `AstDiff::is_empty` before witness
    // qualification could see either container. The persisted ids remain
    // unchanged; this key only preserves every physical definition through
    // the extraction boundary, in deterministic `(id,start,end,AST-order)`
    // order. The ordinal is necessary because distinct one-line definitions
    // can share the same persisted line span.
    let mut nodes: BTreeMap<(String, i64, i64, usize), NodeRow> = BTreeMap::new();
    let mut node_occurrence = 0usize;
    let mut edges: BTreeMap<(String, String, String), EdgeRow> = BTreeMap::new();
    // X4 adversarial review, Finding 4; ALL-distinct-chains as of the Codex
    // round 4 adversarial review, Finding 1: keyed identically to `edges`
    // — `(src_id, dst_id, kind)` — see `GraphFragment::call_scope_chains`'s
    // doc comment. Every physical occurrence's OWN chain is recorded (never
    // just the first), so an edge aggregating calls from multiple distinct
    // scopes carries the full set the X4 resolver tier needs to universally
    // quantify over.
    let mut call_scope_chains: BTreeMap<(String, String, String), BTreeSet<String>> =
        BTreeMap::new();
    // Codex round 5 adversarial review: per-SITE (legacy_src, current_src,
    // callee, kind) pairs — see `GraphFragment::call_attribution_pairs`'s
    // doc comment. Built alongside `call_scope_chains` from the SAME sites,
    // never a second pass.
    let mut call_attribution_pairs: Vec<(String, String, String, String)> = Vec::new();

    // Whole-file content hash (WCR truth pass, Codex round 7 adversarial
    // review): computed ONCE per extraction and stamped onto EVERY edge this
    // parse produces (`add_edge`, below) as `EdgeRow::src_content_hash` —
    // immutable write-time provenance for the WCR re-point gate
    // (`eval::codegraph::historical_src_content_unchanged`), deliberately
    // independent of `code_nodes.body_hash` (which a later `upsert_node`
    // call can refresh out from under a stale edge, in a separate
    // transaction — see that gate's doc comment for the full finding).
    // Reuses the SAME hash computed for the module node's own `body_hash`,
    // immediately below — never a second hashing scheme.
    let file_hash = body_hash(source);

    // Synthetic module node anchoring the file.
    let module_id = node_id(repo, file, "module", file);
    nodes.insert(
        (module_id.clone(), 0, 0, node_occurrence),
        NodeRow {
            id: module_id.clone(),
            repo: repo.into(),
            project: project.into(),
            file: file.into(),
            lang: lang_name.into(),
            kind: "module".into(),
            name: file.into(),
            fqname: file.into(),
            body_hash: file_hash.clone(),
            span_start: 0,
            span_end: 0,
            first_conv_id: conv_id.into(),
            last_conv_id: conv_id.into(),
            last_session_id: session_id.into(),
            // Extraction doesn't know repo identity — populated by the
            // write-path caller (WP2 Stage 1; see
            // `extraction::repo_root::repo_root_for_file`), never guessed
            // here.
            repo_root: None,
            // Extracted definition node — always definition-backed.
            name_only: false,
            // Extraction doesn't know attribution either — populated by
            // the WP2 Stage 2 backfill/hook write path.
            attribution: String::new(),
        },
    );

    let grep = lang.ast_grep(source);
    let root = grep.root();
    // Built ONCE from this file's own `root` and shared by the calls/imports
    // loop below AND `collect_local_bindings_from_root` at the end of this
    // function — see `scope_chain`'s doc comment on why bindings and
    // call/import-site chains must agree on the SAME within-parse indices.
    let func_index = func_node_preorder_index(&root, lang);

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
            // Extraction doesn't know repo identity — populated by the
            // write-path caller (WP2 Stage 1).
            repo_root: None,
            // Extracted definition node — always definition-backed.
            name_only: false,
            // Extraction doesn't know attribution either — populated by
            // the WP2 Stage 2 backfill/hook write path.
            attribution: String::new(),
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
                    src_content_hash: file_hash.clone(),
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
                    node_occurrence += 1;
                    nodes.insert((nid.clone(), start, end, node_occurrence), node);
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
                node_occurrence += 1;
                nodes.insert((nid.clone(), start, end, node_occurrence), node);
                add_edge(module_id.clone(), nid, "defines", 1, "", "");
            }
        }
    }

    // Imports -> imports edge from module (placeholder dst per imported symbol).
    for kind in import_kinds(lang) {
        let matcher = KindMatcher::new(kind, lang);
        for n in root.find_all(&matcher) {
            // Finding 4 (X4 adversarial review): one chain per import
            // STATEMENT node — every symbol in `import { a, b } from 'x'`
            // shares it. Import statements are always module-level in
            // every supported language's grammar today, so this is `""` in
            // practice, but computed generically (never assumed) for the
            // same reason `calls` computes its own per-site chain below.
            let import_chain = scope_chain(&n, lang, &func_index);
            for (sym, module) in import_symbols(&n, lang) {
                if is_noise_callee(&sym) {
                    continue;
                }
                let evidence = if module.is_empty() {
                    String::new()
                } else {
                    format!("from:{}", truncate_chars(&module, MODULE_EVIDENCE_MAX_LEN))
                };
                let dst = format!("name:{sym}");
                add_edge(module_id.clone(), dst.clone(), "imports", 0, "", &evidence);
                call_scope_chains
                    .entry((module_id.clone(), dst, "imports".to_string()))
                    .or_default()
                    .insert(import_chain.clone());
                // Imports are ALWAYS module-sourced (no ancestor walk, old
                // rule or new — see `add_edge` call directly above), so
                // legacy and current attribution are trivially identical.
                // Populated anyway for symmetry with `calls`, immediately
                // below — never special-cased.
                call_attribution_pairs.push((
                    file.to_string(),
                    file.to_string(),
                    sym,
                    "imports".to_string(),
                ));
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
            // TASK 1 (WCR truth pass, "await-glue" bug): unwrap a TS/JS
            // generic-call-after-await `await_expression` function-field
            // artifact BEFORE any callee-text helper runs — see
            // `unwrap_await_glued_function`'s doc comment. A no-op for every
            // other shape.
            let func = unwrap_await_glued_function(func);
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
            // Walk ancestors for the nearest DEFINITION node (WCR truth-pass
            // X4 remediation): shares `nearest_def_node` with the
            // local-binding scope tagger below it in this file, so a call
            // made from inside a closure (the overwhelmingly common shape in
            // idiomatic React hook code — `useCallback`/`useEffect`/
            // `useFocusEffect` bodies) attributes to the SAME enclosing
            // definition its sibling local-binding witnesses are scoped to,
            // rather than silently falling back to module scope the moment
            // the nearest function-kind ancestor happens to be anonymous —
            // see `nearest_def_node`'s doc comment for the real-world TSX
            // repro this fixes.
            let (src, current_src_name) = match nearest_def_node(&n, lang) {
                Some((kind, def_name)) => (node_id(repo, file, kind, &def_name), def_name),
                None => (module_id.clone(), file.to_string()),
            };
            // Codex round 5: the SAME site's attribution under the FROZEN
            // pre-92179d1 rule — see `legacy_src_attribution`'s and
            // `GraphFragment::call_attribution_pairs`'s doc comments.
            // Deliberately independent of `src`/`current_src_name` above,
            // same reasoning as `scope_chain` vs `nearest_def_node` below.
            let legacy_src_name = match legacy_src_attribution(&n, lang) {
                Some((_, name)) => name,
                None => file.to_string(),
            };
            let evidence = match qualifier {
                Some(q) if !q.is_empty() => {
                    format!("via:{}", truncate_chars(&q, MODULE_EVIDENCE_MAX_LEN))
                }
                _ => String::new(),
            };
            let dst = format!("name:{callee}");
            add_edge(src.clone(), dst.clone(), "calls", 0, callee_kind, &evidence);
            call_attribution_pairs.push((
                legacy_src_name,
                current_src_name,
                callee,
                "calls".to_string(),
            ));
            // Finding 4 (X4 adversarial review): the call SITE's own scope
            // chain — deliberately independent of `src` above (which is the
            // coarse, closures-flattened GRAPH attribution from
            // `nearest_def_node`). `scope_chain` walks the SAME ancestors
            // but also records each anonymous closure crossed along the
            // way, so two calls that share `src` (same enclosing named def)
            // but sit in different sibling closures still get DIFFERENT
            // chains — see `GraphFragment::call_scope_chains`'s doc comment.
            // Codex round 4, Finding 1: inserted into the chain SET, never
            // `or_insert_with` — a repeat `(src, dst, "calls")` key (a
            // second physical call site aggregated into the same edge) adds
            // its OWN chain alongside any already recorded, rather than
            // being silently dropped in favor of whichever site was walked
            // first.
            call_scope_chains
                .entry((src, dst, "calls".to_string()))
                .or_default()
                .insert(scope_chain(&n, lang, &func_index));
        }
    }

    // TASK 2 (WCR truth pass X4 tier): local-binding names, from the SAME
    // parsed `root` above and the SAME `func_index` the calls loop just
    // used — never a second parse, never a second index pass.
    let local_bindings = collect_local_bindings_from_root(&root, lang, &func_index);
    let containers = container_spans_from_root(&root, lang);
    // Finding 3 (X4 adversarial review): computed from the SAME parsed
    // `root` — see `tree_has_error`'s doc comment.
    let parse_clean = !tree_has_error(&root);

    GraphFragment {
        nodes: nodes.into_values().collect(),
        edges: edges.into_values().collect(),
        containers,
        local_bindings,
        call_scope_chains,
        call_attribution_pairs,
        parse_clean,
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
#[path = "codegraph_tests.rs"]
mod tests;
