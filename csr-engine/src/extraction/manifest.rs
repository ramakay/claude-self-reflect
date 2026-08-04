//! Manifest-derived external namespaces for the WCR resolver's X1 tier.
//!
//! Extraction (`extraction::codegraph::import_symbols`) records only the
//! *bound local name* of an import (`Arc`, `useState`, `fs`) — never the
//! source module path (`std::sync::Arc`, `'react'`, `'node:fs'`). That means
//! X1 ("external-witnessed") cannot check "is module M external" the way the
//! WCR spec first describes it; it degrades to "does the bound symbol name
//! itself match a manifest-declared dependency or a known stdlib/builtin
//! module name". See [`ExternalNs::classify`] for the exact, conservative
//! rules and what evidence string each subcase produces.
//!
//! Manifests are located by walking up from a source file's directory to the
//! nearest `Cargo.toml` / `package.json` (stopping at the first directory
//! that has either, or at a `.git` boundary if neither is found first).
//! Results are cached per starting directory so a resolve pass touching many
//! files in the same crate/package only walks + parses once.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Declared external namespaces reachable from a given source file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExternalNs {
    /// Crate names from the nearest Cargo.toml's `[dependencies]` +
    /// `[dev-dependencies]` + `[workspace.dependencies]`, as declared
    /// (original casing/spelling — normalization happens at match time).
    pub rust_deps: BTreeSet<String>,
    /// `dependencies` + `devDependencies` keys from the nearest package.json.
    pub js_deps: BTreeSet<String>,
    /// Node.js builtin module names (constant list, always populated).
    pub node_builtins: BTreeSet<String>,
    /// Python stdlib top-level module names (constant list, always populated).
    pub py_stdlib: BTreeSet<String>,
}

const NODE_BUILTINS: &[&str] = &[
    "fs",
    "path",
    "http",
    "url",
    "crypto",
    "os",
    "child_process",
    "util",
    "stream",
    "events",
    "net",
    "zlib",
    "readline",
    "assert",
];

const PY_STDLIB: &[&str] = &[
    "os",
    "sys",
    "json",
    "re",
    "pathlib",
    "typing",
    "datetime",
    "collections",
    "subprocess",
    "asyncio",
    "math",
    "itertools",
    "functools",
    "logging",
    "unittest",
    "time",
    "random",
    "hashlib",
    "shutil",
    "tempfile",
    "threading",
    "dataclasses",
    "enum",
    "abc",
    "io",
    "csv",
    "argparse",
    "urllib",
    "http",
];

/// Rust's own namespace segments — never a project dependency.
const RUST_BUILTIN_NAMESPACES: &[&str] = &["std", "core", "alloc", "proc_macro"];

/// X0 tier (WCR Phase 5): Rust prelude items (`std::prelude::v1`, re-exported
/// automatically into every crate) plus the small set of always-available
/// macros documented in the Rust reference's "Item declarations" / "Macros by
/// example" chapters (`println!`, `vec!`, `assert!`, ...) and core lang items
/// (`Ok`/`Err`/`Some`/`None`, `Drop`, `From`/`Into`, ...). These are never
/// `use`-imported — they are simply always in scope — so the X1 tier (which
/// requires import evidence) can never classify them. Source: the Rust
/// standard library prelude (`std::prelude::v1` / `core::prelude::v1`) and
/// the Rust reference's built-in macro list, as of the 2021/2024 editions.
const RUST_BUILTINS: &[&str] = &[
    "Ok",
    "Err",
    "Some",
    "None",
    "String",
    "Vec",
    "Box",
    "Default",
    "Clone",
    "Copy",
    "Drop",
    "From",
    "Into",
    "TryFrom",
    "TryInto",
    "Iterator",
    "IntoIterator",
    "Option",
    "Result",
    "Send",
    "Sync",
    "Sized",
    "PartialEq",
    "Eq",
    "PartialOrd",
    "Ord",
    "Hash",
    "Debug",
    "Display",
    "ToString",
    "AsRef",
    "AsMut",
    "DoubleEndedIterator",
    "ExactSizeIterator",
    "Extend",
    "drop",
    "format",
    "println",
    "eprintln",
    "print",
    "eprint",
    "write",
    "writeln",
    "vec",
    "panic",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "matches",
    "todo",
    "unimplemented",
    "unreachable",
    "include_str",
    "include_bytes",
    "concat",
    "stringify",
    "env",
    "option_env",
    "file",
    "line",
    "column",
    "cfg",
    "compile_error",
];

/// X0 tier: JS/TS/Node global identifiers — Web API globals (per MDN's
/// "Standard built-in objects" + Window globals) plus Node.js globals
/// (`process`, `Buffer`, `require`, `module`, `__dirname`, `__filename`) that
/// are ambient in every module, never `import`ed.
const JS_GLOBALS: &[&str] = &[
    "fetch",
    "console",
    "JSON",
    "Math",
    "Date",
    "Promise",
    "Array",
    "Object",
    "String",
    "Number",
    "Boolean",
    "Map",
    "Set",
    "WeakMap",
    "WeakSet",
    "Symbol",
    "Proxy",
    "Reflect",
    "RegExp",
    "Error",
    "TypeError",
    "RangeError",
    "parseInt",
    "parseFloat",
    "isNaN",
    "isFinite",
    "encodeURIComponent",
    "decodeURIComponent",
    "structuredClone",
    "setTimeout",
    "setInterval",
    "clearTimeout",
    "clearInterval",
    "queueMicrotask",
    "requestAnimationFrame",
    "atob",
    "btoa",
    "alert",
    "document",
    "window",
    "navigator",
    "localStorage",
    "sessionStorage",
    "URL",
    "URLSearchParams",
    "AbortController",
    "TextEncoder",
    "TextDecoder",
    "Intl",
    "BigInt",
    "globalThis",
    "require",
    "module",
    "process",
    "Buffer",
    "__dirname",
    "__filename",
];

/// X0 tier: Python builtins — the `builtins` module's always-in-scope names
/// (functions + exception types), per the CPython "Built-in Functions" /
/// "Built-in Exceptions" reference pages. Never `import`ed.
const PY_BUILTINS: &[&str] = &[
    "print",
    "len",
    "str",
    "int",
    "float",
    "bool",
    "list",
    "dict",
    "set",
    "tuple",
    "range",
    "enumerate",
    "zip",
    "map",
    "filter",
    "sorted",
    "reversed",
    "sum",
    "min",
    "max",
    "abs",
    "round",
    "open",
    "input",
    "isinstance",
    "issubclass",
    "hasattr",
    "getattr",
    "setattr",
    "super",
    "type",
    "id",
    "repr",
    "format",
    "iter",
    "next",
    "vars",
    "dir",
    "hash",
    "callable",
    "staticmethod",
    "classmethod",
    "property",
    "Exception",
    "ValueError",
    "TypeError",
    "KeyError",
    "IndexError",
    "RuntimeError",
    "StopIteration",
    "FileNotFoundError",
];

/// X0 tier: Go predeclared identifiers — the language spec's "Predeclared
/// identifiers" section (builtin functions + basic types). Never `import`ed.
const GO_BUILTINS: &[&str] = &[
    "make", "new", "len", "cap", "append", "copy", "delete", "panic", "recover", "close", "print",
    "println", "error", "string", "int", "int32", "int64", "uint", "uint32", "uint64", "float32",
    "float64", "bool", "byte", "rune", "complex", "real", "imag", "any",
];

/// Classify `name` against the X0 (builtin/prelude/global) list for
/// `lang_key` (`"rust"` | `"js"` | `"python"` | `"go"`). Case-sensitive exact
/// match only — these are language-defined names witnessed by the language
/// spec/stdlib docs cited on each list above, not assumptions. Returns
/// `false` for an unrecognized `lang_key`.
pub fn classify_builtin(lang_key: &str, name: &str) -> bool {
    match lang_key {
        "rust" => RUST_BUILTINS.contains(&name),
        "js" => JS_GLOBALS.contains(&name),
        "python" => PY_BUILTINS.contains(&name),
        "go" => GO_BUILTINS.contains(&name),
        _ => false,
    }
}

/// Map a source file's extension to the X0 lang key used by
/// `classify_builtin`. `None` for extensions with no builtin list above.
pub fn builtin_lang_key_from_file(file: &str) -> Option<&'static str> {
    let ext = file.rsplit('.').next()?.to_lowercase();
    match ext.as_str() {
        "rs" => Some("rust"),
        "ts" | "tsx" | "js" | "jsx" | "mjs" => Some("js"),
        "py" => Some("python"),
        "go" => Some("go"),
        _ => None,
    }
}

type ManifestCache = Mutex<std::collections::BTreeMap<PathBuf, ExternalNs>>;
static CACHE: OnceLock<ManifestCache> = OnceLock::new();

fn cache() -> &'static ManifestCache {
    CACHE.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
}

impl ExternalNs {
    /// Classify a bound symbol name as external, returning the evidence
    /// module string (`import:<M>`'s `<M>`) when it matches, `None` when the
    /// name cannot be attributed to any known namespace.
    ///
    /// Match order (first hit wins), all conservative-by-construction:
    /// 1. Exact match against a builtin list (Node builtin, Python stdlib
    ///    top-level module, or a Rust namespace segment `std`/`core`/`alloc`/
    ///    `proc_macro`). Case-sensitive — builtins are lowercase by
    ///    convention and so is the AST-captured identifier for them.
    /// 2. Exact match against a manifest dependency name, hyphen/underscore
    ///    normalized (`async-trait` == `async_trait`) so the Rust `use`-path
    ///    spelling matches the Cargo.toml key spelling.
    /// 3. "Start-uppercase-agnostic" exact match against a manifest
    ///    dependency name: fold only the *first* character's case before
    ///    comparing (`Regex` vs `regex`), covering the common
    ///    crate/package-name-as-type-name convention. Everything after the
    ///    first character must match exactly — this is deliberately narrow,
    ///    not a general case-insensitive compare, so it doesn't over-match.
    pub fn classify(&self, symbol: &str) -> Option<String> {
        if symbol.is_empty() {
            return None;
        }

        // 1. Builtins — exact match.
        if self.node_builtins.contains(symbol) {
            return Some(symbol.to_string());
        }
        // Defensive only: extraction never captures a `node:`-prefixed bound
        // name (identifiers can't contain `:`), but honor it if the schema
        // ever starts recording module specifiers instead of bound names.
        if let Some(stripped) = symbol.strip_prefix("node:") {
            if self.node_builtins.contains(stripped) {
                return Some(symbol.to_string());
            }
        }
        if self.py_stdlib.contains(symbol) {
            return Some(symbol.to_string());
        }
        if RUST_BUILTIN_NAMESPACES.contains(&symbol) {
            return Some(symbol.to_string());
        }

        // 2. Manifest deps — hyphen/underscore-normalized exact match.
        let norm_symbol = normalize_dep(symbol);
        for dep in self.rust_deps.iter().chain(self.js_deps.iter()) {
            if normalize_dep(dep) == norm_symbol {
                return Some(dep.clone());
            }
        }

        // 3. Manifest deps — start-uppercase-agnostic exact match.
        let folded_symbol = fold_first_char(symbol);
        for dep in self.rust_deps.iter().chain(self.js_deps.iter()) {
            if fold_first_char(dep) == folded_symbol {
                return Some(dep.clone());
            }
        }

        None
    }

    /// X1 module-aware tier (WCR Phase 5): classify a bare (non-relative)
    /// import *module specifier* (e.g. `react`, `node:fs`, `std::collections`,
    /// `os.path` — the full `from:<module>` string captured at extraction
    /// time by `extraction::codegraph::import_symbols`, NOT the bound symbol
    /// name). Splits off the module's first path segment (before the first
    /// `/`, `:`, or `.` — covering JS `pkg/sub`, Rust `std::path`, and Python
    /// `os.path` in one pass) and matches that segment the same way
    /// [`classify`] matches a bound symbol: builtins first, then
    /// manifest deps (hyphen/underscore then start-uppercase-agnostic).
    /// Callers must check [`is_relative_module`] first — a relative
    /// specifier is an internal-candidate, never external, and this method
    /// does not special-case it.
    pub fn classify_module(&self, module: &str) -> bool {
        let stripped = module.strip_prefix("node:").unwrap_or(module);
        let segment = first_path_segment(stripped);
        if segment.is_empty() {
            return false;
        }
        if self.node_builtins.contains(segment) {
            return true;
        }
        if self.py_stdlib.contains(segment) {
            return true;
        }
        if RUST_BUILTIN_NAMESPACES.contains(&segment) {
            return true;
        }

        // Manifest dep matching: a scoped npm package (`@scope/name`) is
        // keyed in package.json by its FULL two-segment name — the plain
        // `first_path_segment` above only yields `@scope` (everything before
        // the first `/`), which never matches `@scope/name` (WCR Phase 6,
        // TASK E). `npm_package_key` returns the full `@scope/name` for a
        // scoped specifier and falls back to `segment`-equivalent behavior
        // for everything else (unscoped npm packages, Rust crate paths).
        let dep_key = npm_package_key(stripped);
        let norm_key = normalize_dep(dep_key);
        if self
            .rust_deps
            .iter()
            .chain(self.js_deps.iter())
            .any(|dep| normalize_dep(dep) == norm_key)
        {
            return true;
        }
        let folded_key = fold_first_char(dep_key);
        self.rust_deps
            .iter()
            .chain(self.js_deps.iter())
            .any(|dep| fold_first_char(dep) == folded_key)
    }
}

/// The npm package-name "key" for a module specifier used for manifest dep
/// matching: for a scoped package (`@scope/name` or `@scope/name/sub/path`)
/// this is the full `@scope/name` two-segment form — matching how
/// package.json's `dependencies`/`devDependencies` key scoped packages —
/// for anything else it's just the first path segment (unscoped npm
/// package, Rust `::`-path, Python `.`-path). WCR Phase 6, TASK E: the
/// previous single-segment split broke on every scoped dependency (`@scope`
/// alone is never a real package.json key).
fn npm_package_key(s: &str) -> &str {
    if !s.starts_with('@') {
        return first_path_segment(s);
    }
    let mut slashes = 0usize;
    for (i, c) in s.char_indices() {
        if c == '/' {
            slashes += 1;
            if slashes == 2 {
                return &s[..i];
            }
        }
    }
    // Zero or one slash: `@scope/name` (whole string is already the key) or
    // a malformed/lone `@scope` (falls back to the whole string — matches
    // nothing real, harmless).
    s
}

/// True for a module specifier that is always an INTERNAL candidate — a
/// relative path (`./foo`, `../foo`) or a tsconfig/webpack-style path alias
/// (`~/foo`, `@/foo`, both common "map this prefix to a project-root-
/// relative import" conventions — WCR Phase 6, TASK E) — never classified
/// external by the X1 module-aware tier.
///
/// `@/foo` is deliberately distinguished from a scoped npm package
/// (`@scope/name`, e.g. `@clerk/expo`): an alias has NOTHING between `@` and
/// the following `/`, a scoped package always has a non-empty scope name
/// there, so this check never swallows a genuine scoped dependency.
pub fn is_relative_module(module: &str) -> bool {
    module.starts_with("./")
        || module.starts_with("../")
        || module.starts_with("~/")
        || module.starts_with("@/")
}

/// The substring of `s` before its first `/`, `:`, or `.` (whichever comes
/// first), or all of `s` when none appear. `/` covers JS/Node/Go path-style
/// specifiers, `:` covers Rust `::` namespacing (and the `node:` prefix,
/// which callers strip before reaching here), `.` covers Python's dotted
/// module notation.
fn first_path_segment(s: &str) -> &str {
    match s.find(['/', ':', '.']) {
        Some(i) => &s[..i],
        None => s,
    }
}

fn normalize_dep(s: &str) -> String {
    s.replace('-', "_")
}

fn fold_first_char(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Resolve the external namespaces reachable from `project_file` by walking
/// up its ancestor directories to the nearest `Cargo.toml` / `package.json`.
/// Never fails: a missing/unreadable manifest just yields empty dep sets
/// (builtins are always populated). Cached per starting directory.
pub fn external_namespaces(project_file: &Path) -> ExternalNs {
    let Some(start_dir) = project_file.parent() else {
        return builtins_only();
    };
    let key = start_dir.to_path_buf();

    if let Ok(guard) = cache().lock() {
        if let Some(hit) = guard.get(&key) {
            return hit.clone();
        }
    }

    let ns = compute_external_namespaces(&key);

    if let Ok(mut guard) = cache().lock() {
        guard.insert(key, ns.clone());
    }
    ns
}

fn builtins_only() -> ExternalNs {
    ExternalNs {
        rust_deps: BTreeSet::new(),
        js_deps: BTreeSet::new(),
        node_builtins: NODE_BUILTINS.iter().map(|s| s.to_string()).collect(),
        py_stdlib: PY_STDLIB.iter().map(|s| s.to_string()).collect(),
    }
}

/// Walk `start_dir` and its ancestors looking for the nearest `Cargo.toml`
/// and/or `package.json`. Stops at the first directory where either is
/// found (parsing whichever of the two is present there — a mixed
/// Rust+Node repo can have both), or at a `.git` boundary if neither turns
/// up first, so the walk never escapes the repository.
fn compute_external_namespaces(start_dir: &Path) -> ExternalNs {
    let mut ns = builtins_only();
    let mut dir = Some(start_dir.to_path_buf());
    // Bounded walk: a real repo is never this deep; this just guards against
    // pathological input (e.g. a relative path with no real filesystem root
    // nearby) spinning all the way to `/`.
    const MAX_DEPTH: usize = 64;

    for _ in 0..MAX_DEPTH {
        let Some(d) = dir else { break };
        let cargo_path = d.join("Cargo.toml");
        let pkg_path = d.join("package.json");
        let has_cargo = cargo_path.is_file();
        let has_pkg = pkg_path.is_file();

        if has_cargo || has_pkg {
            if has_cargo {
                ns.rust_deps = parse_cargo_deps(&cargo_path);
            }
            if has_pkg {
                ns.js_deps = parse_package_json_deps(&pkg_path);
            }
            break;
        }

        if d.join(".git").exists() {
            // Repo boundary reached with no manifest found — stop here
            // rather than walking into unrelated ancestor directories.
            break;
        }

        dir = d.parent().map(|p| p.to_path_buf());
    }

    ns
}

/// X1 fallback (WCR Phase 6, TASK E): when the direct ancestor walk from a
/// source file finds no manifest at all (`ns.rust_deps` and `ns.js_deps`
/// both empty — `compute_external_namespaces` hit a `.git` boundary or ran
/// out of ancestors before any Cargo.toml/package.json), retry against each
/// of the project's already-known roots (typically
/// `repo_scan::project_roots`, derived independently from `code_nodes.file`
/// — corpus-witnessed evidence of where this project's real root(s) live),
/// first hit wins. This recovers cases where a single file's ancestor walk
/// would stop at an unrelated intermediate `.git`/dead-end before reaching
/// the project's actual manifest, without ever inventing a manifest that
/// isn't really there. Still conservative: when `ns` already has real deps,
/// or none of the fallback roots has a manifest either, `ns` is returned
/// unchanged.
pub fn apply_project_root_fallback(ns: ExternalNs, fallback_roots: &[PathBuf]) -> ExternalNs {
    if !ns.rust_deps.is_empty() || !ns.js_deps.is_empty() {
        return ns;
    }
    for root in fallback_roots {
        let cargo_path = root.join("Cargo.toml");
        let pkg_path = root.join("package.json");
        let has_cargo = cargo_path.is_file();
        let has_pkg = pkg_path.is_file();
        if has_cargo || has_pkg {
            let mut fallback = ns.clone();
            if has_cargo {
                fallback.rust_deps = parse_cargo_deps(&cargo_path);
            }
            if has_pkg {
                fallback.js_deps = parse_package_json_deps(&pkg_path);
            }
            return fallback;
        }
    }
    ns
}

type NodeModulesCache = Mutex<std::collections::BTreeMap<PathBuf, Vec<PathBuf>>>;
static NODE_MODULES_CACHE: OnceLock<NodeModulesCache> = OnceLock::new();

fn node_modules_cache() -> &'static NodeModulesCache {
    NODE_MODULES_CACHE.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
}

/// Every `node_modules` directory that exists along the ancestor walk from
/// `start_dir` up to a `.git` boundary (WCR Phase 7, TASK B) — deliberately
/// NOT "stop at the first one found", unlike `compute_external_namespaces`:
/// a monorepo commonly hoists shared deps into a root-level `node_modules`
/// while a package-local one also exists closer to `start_dir`, and a
/// transitive/bundled dependency (WCR Phase 7, TASK B's motivating case —
/// `@expo/vector-icons` shipped inside `expo`) can land at either level.
/// Cached per starting directory, same memoization pattern as
/// `external_namespaces`.
fn node_modules_dirs(start_dir: &Path) -> Vec<PathBuf> {
    let key = start_dir.to_path_buf();
    if let Ok(guard) = node_modules_cache().lock() {
        if let Some(hit) = guard.get(&key) {
            return hit.clone();
        }
    }

    let mut found = Vec::new();
    let mut dir = Some(start_dir.to_path_buf());
    const MAX_DEPTH: usize = 64;
    for _ in 0..MAX_DEPTH {
        let Some(d) = dir else { break };
        let nm = d.join("node_modules");
        if nm.is_dir() {
            found.push(nm);
        }
        if d.join(".git").exists() {
            break;
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }

    if let Ok(mut guard) = node_modules_cache().lock() {
        guard.insert(key, found.clone());
    }
    found
}

/// TASK B (WCR Phase 7): installed-package witness. A bare (non-relative,
/// non-alias) JS/TS module specifier the nearest package.json does NOT
/// declare directly may still be a real, INSTALLED dependency — a
/// transitive package bundled inside another declared dependency (e.g.
/// `@expo/vector-icons`, shipped inside `expo`'s own `node_modules` tree
/// rather than the app's top-level `dependencies`). Directory existence on
/// disk is verifiable proof of an installed package, independent of
/// whether the immediate manifest happens to list it — never a guess about
/// what's "probably" there. Returns the matched package key (the same
/// `npm_package_key` value used to check disk — scoped packages resolve to
/// their full `@scope/name` two-segment key) on a hit, `None` otherwise.
/// `start_dir` should be the pending edge's `src_file` parent directory;
/// walks every `node_modules` ancestor (see `node_modules_dirs`), not just
/// the nearest.
pub fn node_modules_package_witness(start_dir: &Path, module: &str) -> Option<String> {
    let stripped = module.strip_prefix("node:").unwrap_or(module);
    let pkg = npm_package_key(stripped);
    if pkg.is_empty() {
        return None;
    }
    for dir in node_modules_dirs(start_dir) {
        if dir.join(pkg).is_dir() {
            return Some(pkg.to_string());
        }
    }
    None
}

/// Extract dependency names from `[dependencies]`, `[dev-dependencies]`, and
/// `[workspace.dependencies]` of a Cargo.toml. Malformed/unreadable files
/// yield an empty set rather than propagating an error — manifest parsing is
/// a best-effort signal, never a hard requirement for resolution to proceed.
fn parse_cargo_deps(path: &Path) -> BTreeSet<String> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return BTreeSet::new();
    };
    let Ok(value) = toml::from_str::<toml::Value>(&contents) else {
        return BTreeSet::new();
    };

    let mut out = BTreeSet::new();
    for key in ["dependencies", "dev-dependencies"] {
        if let Some(table) = value.get(key).and_then(toml::Value::as_table) {
            out.extend(table.keys().cloned());
        }
    }
    if let Some(table) = value
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(toml::Value::as_table)
    {
        out.extend(table.keys().cloned());
    }
    out
}

/// Extract dependency names from `dependencies` + `devDependencies` of a
/// package.json. Same best-effort contract as `parse_cargo_deps`.
fn parse_package_json_deps(path: &Path) -> BTreeSet<String> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return BTreeSet::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return BTreeSet::new();
    };

    let mut out = BTreeSet::new();
    for key in ["dependencies", "devDependencies"] {
        if let Some(obj) = value.get(key).and_then(serde_json::Value::as_object) {
            out.extend(obj.keys().cloned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_rust_std_builtin() {
        let ns = builtins_only();
        assert_eq!(ns.classify("std"), Some("std".to_string()));
        assert_eq!(ns.classify("core"), Some("core".to_string()));
    }

    #[test]
    fn classify_node_builtin_exact() {
        let ns = builtins_only();
        assert_eq!(ns.classify("fs"), Some("fs".to_string()));
        assert_eq!(ns.classify("not_a_builtin"), None);
    }

    #[test]
    fn classify_python_stdlib_exact() {
        let ns = builtins_only();
        assert_eq!(ns.classify("os"), Some("os".to_string()));
        assert_eq!(ns.classify("json"), Some("json".to_string()));
    }

    #[test]
    fn classify_rust_dep_hyphen_underscore_normalized() {
        let mut ns = builtins_only();
        ns.rust_deps.insert("async-trait".to_string());
        assert_eq!(
            ns.classify("async_trait"),
            Some("async-trait".to_string()),
            "hyphen dep name must match underscore-spelled use path"
        );
    }

    #[test]
    fn classify_start_uppercase_agnostic_dep_match() {
        let mut ns = builtins_only();
        ns.rust_deps.insert("regex".to_string());
        assert_eq!(ns.classify("Regex"), Some("regex".to_string()));
        // Only the first char folds — a fully different spelling must not match.
        assert_eq!(ns.classify("REGEX"), None);
    }

    #[test]
    fn classify_no_match_is_none() {
        let ns = builtins_only();
        assert_eq!(ns.classify("totally_unknown_symbol"), None);
    }

    #[test]
    fn parse_cargo_deps_reads_all_three_sections() {
        let tmp = tempfile::tempdir().unwrap();
        let cargo_toml = tmp.path().join("Cargo.toml");
        std::fs::write(
            &cargo_toml,
            r#"
[package]
name = "x"

[dependencies]
serde = "1"

[dev-dependencies]
tempfile = "3"

[workspace.dependencies]
tokio = "1"
"#,
        )
        .unwrap();
        let deps = parse_cargo_deps(&cargo_toml);
        assert!(deps.contains("serde"));
        assert!(deps.contains("tempfile"));
        assert!(deps.contains("tokio"));
    }

    #[test]
    fn parse_package_json_deps_reads_both_sections() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("package.json");
        std::fs::write(
            &pkg,
            r#"{"dependencies": {"react": "18"}, "devDependencies": {"vitest": "1"}}"#,
        )
        .unwrap();
        let deps = parse_package_json_deps(&pkg);
        assert!(deps.contains("react"));
        assert!(deps.contains("vitest"));
    }

    #[test]
    fn external_namespaces_walks_up_to_nearest_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src/nested")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname=\"x\"\n[dependencies]\nanyhow=\"1\"\n",
        )
        .unwrap();
        let file = root.join("src/nested/deep.rs");
        std::fs::write(&file, "// nothing").unwrap();

        let ns = external_namespaces(&file);
        assert!(ns.rust_deps.contains("anyhow"));
        // Builtins are always populated regardless of manifest contents.
        assert!(ns.node_builtins.contains("fs"));
    }

    #[test]
    fn external_namespaces_no_manifest_is_builtins_only() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("orphan.rs");
        std::fs::write(&file, "// nothing").unwrap();
        // No Cargo.toml/package.json anywhere under this fresh tempdir, and
        // no .git either — the walk runs out of ancestors and stops.
        let ns = external_namespaces(&file);
        assert!(ns.rust_deps.is_empty());
        assert!(ns.js_deps.is_empty());
    }

    // ─── X0 builtin/prelude/global tier ───

    #[test]
    fn classify_builtin_rust_prelude_item() {
        assert!(classify_builtin("rust", "Ok"));
        assert!(classify_builtin("rust", "Default"));
        assert!(!classify_builtin("rust", "totally_unknown_symbol"));
    }

    #[test]
    fn classify_builtin_js_global() {
        assert!(classify_builtin("js", "fetch"));
        assert!(classify_builtin("js", "console"));
        assert!(!classify_builtin("js", "totally_unknown_symbol"));
    }

    #[test]
    fn classify_builtin_python_builtin() {
        assert!(classify_builtin("python", "print"));
        assert!(classify_builtin("python", "ValueError"));
        assert!(!classify_builtin("python", "totally_unknown_symbol"));
    }

    #[test]
    fn classify_builtin_go_predeclared() {
        assert!(classify_builtin("go", "make"));
        assert!(classify_builtin("go", "len"));
        assert!(!classify_builtin("go", "totally_unknown_symbol"));
    }

    #[test]
    fn classify_builtin_case_sensitive_exact_match_only() {
        // Lowercase "ok" is not the Rust prelude item `Ok`.
        assert!(!classify_builtin("rust", "ok"));
    }

    #[test]
    fn builtin_lang_key_from_file_maps_extensions() {
        assert_eq!(builtin_lang_key_from_file("src/a.rs"), Some("rust"));
        assert_eq!(builtin_lang_key_from_file("src/a.ts"), Some("js"));
        assert_eq!(builtin_lang_key_from_file("src/a.tsx"), Some("js"));
        assert_eq!(builtin_lang_key_from_file("src/a.js"), Some("js"));
        assert_eq!(builtin_lang_key_from_file("src/a.py"), Some("python"));
        assert_eq!(builtin_lang_key_from_file("src/a.go"), Some("go"));
        assert_eq!(builtin_lang_key_from_file("src/a.rb"), None);
    }

    // ─── X1 module-aware tier ───

    #[test]
    fn is_relative_module_true_for_dot_slash_and_dot_dot_slash() {
        assert!(is_relative_module("./util"));
        assert!(is_relative_module("../util"));
        assert!(!is_relative_module("util"));
        assert!(!is_relative_module("node:fs"));
    }

    #[test]
    fn classify_module_bare_js_dep_matches_manifest() {
        let mut ns = builtins_only();
        ns.js_deps.insert("react".to_string());
        assert!(ns.classify_module("react"));
        assert!(!ns.classify_module("totally-unknown-package"));
    }

    #[test]
    fn classify_module_node_builtin_with_and_without_prefix() {
        let ns = builtins_only();
        assert!(ns.classify_module("fs"));
        assert!(ns.classify_module("node:fs"));
        assert!(ns.classify_module("node:fs/promises"));
    }

    #[test]
    fn classify_module_rust_std_path_matches_first_segment() {
        let ns = builtins_only();
        assert!(ns.classify_module("std::collections"));
        assert!(ns.classify_module("core::fmt"));
    }

    #[test]
    fn classify_module_rust_dep_matches_first_segment() {
        let mut ns = builtins_only();
        ns.rust_deps.insert("async-trait".to_string());
        assert!(ns.classify_module("async_trait"));
    }

    #[test]
    fn classify_module_python_dotted_matches_stdlib_top_level() {
        let ns = builtins_only();
        assert!(ns.classify_module("os.path"));
        assert!(!ns.classify_module("totally_unknown.nested"));
    }

    #[test]
    fn classify_module_relative_never_matches_via_module_path() {
        // Relative specifiers are internal candidates. classify_module itself
        // has no relative special-case (callers must check is_relative_module
        // first) — asserted here so a `./`-leading segment (empty first
        // segment) is proven to fail the match rather than silently matching.
        let ns = builtins_only();
        assert!(!ns.classify_module("./util"));
    }

    // ─── Scoped npm packages + tsconfig aliases (WCR Phase 6, TASK E) ───

    #[test]
    fn classify_module_scoped_npm_package_matches_full_two_segment_key() {
        let mut ns = builtins_only();
        ns.js_deps.insert("@clerk/expo".to_string());
        assert!(
            ns.classify_module("@clerk/expo"),
            "scoped package must match its full @scope/name key"
        );
        // A different package under the SAME scope must not false-positive
        // match just because the scope segment collides.
        assert!(!ns.classify_module("@clerk/nextjs"));
    }

    #[test]
    fn classify_module_scoped_npm_package_deep_subpath_matches_package_key() {
        let mut ns = builtins_only();
        ns.js_deps
            .insert("@react-native-async-storage/async-storage".to_string());
        assert!(ns.classify_module("@react-native-async-storage/async-storage"));
        assert!(
            ns.classify_module("@react-native-async-storage/async-storage/lib/deep"),
            "deep subpath import must still match the package-level key"
        );
    }

    #[test]
    fn classify_module_unscoped_package_still_matches_first_segment() {
        let mut ns = builtins_only();
        ns.js_deps.insert("react".to_string());
        assert!(ns.classify_module("react"));
        assert!(ns.classify_module("react/jsx-runtime"));
    }

    #[test]
    fn is_relative_module_true_for_tsconfig_style_aliases() {
        assert!(is_relative_module("~/utils/helper"));
        assert!(is_relative_module("@/components/Button"));
        // A genuine scoped npm package must NOT be treated as an alias — the
        // scope segment is non-empty between `@` and `/`.
        assert!(!is_relative_module("@clerk/expo"));
        assert!(!is_relative_module("@expo/vector-icons"));
    }

    #[test]
    fn project_root_fallback_recovers_manifest_when_direct_walk_finds_none() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("real_root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"dependencies": {"@expo/vector-icons": "1.0.0"}}"#,
        )
        .unwrap();

        // `ns` as if the direct ancestor walk found nothing (orphan file,
        // e.g. a scratchpad script with no manifest anywhere on its own path).
        let ns = builtins_only();
        assert!(
            ns.js_deps.is_empty(),
            "fixture assumes an empty starting ns"
        );

        let fallback_roots = vec![root.clone()];
        let recovered = apply_project_root_fallback(ns, &fallback_roots);
        assert!(recovered.js_deps.contains("@expo/vector-icons"));
        assert!(recovered.classify_module("@expo/vector-icons"));
    }

    #[test]
    fn project_root_fallback_leaves_ns_unchanged_when_direct_walk_already_found_deps() {
        let mut ns = builtins_only();
        ns.js_deps.insert("react".to_string());
        let fallback_roots = vec![PathBuf::from("/definitely/does/not/matter")];
        let result = apply_project_root_fallback(ns.clone(), &fallback_roots);
        assert_eq!(result, ns, "direct-walk result must win, fallback unused");
    }

    #[test]
    fn project_root_fallback_returns_unchanged_when_no_root_has_a_manifest_either() {
        let tmp = tempfile::tempdir().unwrap();
        let empty_root = tmp.path().join("no_manifest_here");
        std::fs::create_dir_all(&empty_root).unwrap();

        let ns = builtins_only();
        let result = apply_project_root_fallback(ns.clone(), &[empty_root]);
        assert_eq!(
            result, ns,
            "no manifest anywhere -> unchanged, never invented"
        );
    }

    // ─── Installed-package witness (WCR Phase 7, TASK B) ───

    #[test]
    fn node_modules_package_witness_finds_scoped_package_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("app");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"dependencies": {"expo": "1.0.0"}}"#,
        )
        .unwrap();
        // `@expo/vector-icons` is NOT a direct dependency — it's bundled
        // transitively inside `expo`'s own node_modules tree, exactly the
        // "Feather -> @expo/vector-icons" motivating case.
        let pkg_dir = root.join("node_modules/@expo/vector-icons");
        std::fs::create_dir_all(&pkg_dir).unwrap();

        let hit = node_modules_package_witness(&src, "@expo/vector-icons");
        assert_eq!(hit, Some("@expo/vector-icons".to_string()));
    }

    #[test]
    fn node_modules_package_witness_finds_unscoped_package_and_deep_subpath() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("app");
        let src = root.join("src/nested");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(root.join("node_modules/lodash")).unwrap();

        assert_eq!(
            node_modules_package_witness(&src, "lodash"),
            Some("lodash".to_string())
        );
        assert_eq!(
            node_modules_package_witness(&src, "lodash/debounce"),
            Some("lodash".to_string()),
            "deep subpath import must still match the package-level directory"
        );
    }

    #[test]
    fn node_modules_package_witness_none_when_directory_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("app_empty");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("package.json"), "{}").unwrap();

        assert_eq!(
            node_modules_package_witness(&root, "totally-not-installed"),
            None
        );
    }

    #[test]
    fn node_modules_package_witness_checks_every_ancestor_level_not_just_nearest() {
        let tmp = tempfile::tempdir().unwrap();
        let monorepo_root = tmp.path().join("monorepo");
        let pkg_dir = monorepo_root.join("packages/app/src");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        // Hoisted dependency lives at the MONOREPO root's node_modules, two
        // levels above the source file — not adjacent to it.
        std::fs::create_dir_all(monorepo_root.join("node_modules/react")).unwrap();

        assert_eq!(
            node_modules_package_witness(&pkg_dir, "react"),
            Some("react".to_string())
        );
    }
}
