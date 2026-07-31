//! On-disk repo definition scanner (Witness-Closure Resolution, Phase 2).
//!
//! Phase 1 gave the resolver a `repo_defs` table: an independent, ground-truth
//! inventory of `(project, file, name, kind, lang)` rows, separate from
//! `code_nodes` (which only contains symbols CSR has *seen* through hook /
//! import activity). This module fills that table by walking a project's
//! source tree directly and reusing the codegraph extractor per file.
//!
//! Walk is deliberately **serial** — `rusqlite::Connection` is not `Sync`, and
//! deterministic scan order matters for reproducible `repo_defs` snapshots.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use ignore::WalkBuilder;
use rusqlite::{params, Connection};

use super::codegraph::extract_graph_fragment_for_file;
use super::manifest;
use super::repo_path::canonical_repo_path;
use crate::storage::codegraph::upsert_repo_defs;

/// Directory names never worth walking into, regardless of `.gitignore` state.
/// Most of these are also caught by the walker's default hidden-file filter
/// (`.git`), but build/vendor dirs like `node_modules` or `target` are not
/// dot-prefixed, so they need an explicit skip.
const HARD_SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    "venv",
    "__pycache__",
];

/// Extensions recognized as scannable source.
///
/// `swift` is listed for parity with the spec's language matrix, but this
/// build has no `SupportLang::Swift` — `ast-grep-language`'s
/// `tree-sitter-swift` feature is not enabled in `Cargo.toml` (only python,
/// typescript, rust, javascript, go are). `.swift` files are still walked and
/// counted in `files_scanned`, but `extract_graph_fragment_for_file` returns
/// an empty fragment for them (its internal `lang_from_path_str` yields
/// `None`), so they never contribute defs. Documented deviation, not a bug.
const CODE_EXTENSIONS: &[&str] = &["rs", "ts", "tsx", "js", "jsx", "py", "go", "swift"];

/// Per-file size cap: files larger than this are never read or extracted.
const MAX_FILE_BYTES: u64 = 512 * 1024;

/// Per-project scan cap: stop after this many files have been scanned.
const MAX_FILES_PER_PROJECT: usize = 5000;

/// A single line longer than this is treated as evidence the file is
/// minified/bundled rather than hand-written source (WCR Phase 6, TASK D).
/// Real hand-written lines — even long ones (long string literals, wide
/// match arms) — essentially never cross this; minifiers routinely collapse
/// an entire module onto one line far past it.
const MINIFIED_LINE_LEN_THRESHOLD: usize = 2000;

/// Result of a `scan_project` / `scan_all` run.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScanStats {
    pub files_scanned: usize,
    pub defs_indexed: usize,
    pub skipped_large: usize,
    /// Files skipped because they look minified/bundled (WCR Phase 6, TASK
    /// D) — see `looks_minified`. Vendored/bundled JS commonly slips past
    /// `HARD_SKIP_DIRS` when it's checked into the repo outside
    /// node_modules/dist/build (e.g. a committed `vendor.min.js`), and its
    /// mangled short identifiers or single-word helper names would otherwise
    /// pollute `repo_defs` with shadow noise.
    pub skipped_minified: usize,
    /// Def rows dropped because their name exactly matches a language
    /// builtin/global (`manifest::classify_builtin` — WCR Phase 6, TASK D).
    /// A repo "defining" a language builtin (a vendored file re-exporting
    /// something literally named `fetch`, or a bundle whose mangled output
    /// happens to define `slice`) is shadow noise, not a real user symbol:
    /// left in `repo_defs` it creates a spurious 2nd (or wrongly-bound 1st)
    /// candidate that blocks the resolver's X0 builtin tier and X2
    /// method-call tier from ever firing for that name.
    pub builtin_defs_skipped: usize,
    pub truncated: bool,
    pub duration_ms: u64,
}

impl ScanStats {
    /// Sum two stats runs (used by `scan_all` across multiple roots).
    /// `truncated` is sticky: true if either run truncated.
    fn merge(&mut self, other: &ScanStats) {
        self.files_scanned += other.files_scanned;
        self.defs_indexed += other.defs_indexed;
        self.skipped_large += other.skipped_large;
        self.skipped_minified += other.skipped_minified;
        self.builtin_defs_skipped += other.builtin_defs_skipped;
        self.truncated = self.truncated || other.truncated;
        self.duration_ms += other.duration_ms;
    }
}

/// True when `path`/`source` looks minified/bundled rather than
/// hand-written: a `.min.js` filename, or any single line over
/// `MINIFIED_LINE_LEN_THRESHOLD` characters.
fn looks_minified(path: &Path, source: &str) -> bool {
    let min_js_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.ends_with(".min.js"))
        .unwrap_or(false);
    min_js_name
        || source
            .lines()
            .any(|line| line.len() > MINIFIED_LINE_LEN_THRESHOLD)
}

fn has_code_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            CODE_EXTENSIONS
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(e))
        })
        .unwrap_or(false)
}

fn is_hard_skip_dir(name: &str) -> bool {
    HARD_SKIP_DIRS.contains(&name)
}

/// Walk `root`, extract definitions from every recognized source file, and
/// upsert them into `repo_defs` (per-file replace semantics — a rescanned
/// file never leaves stale defs behind).
///
/// `.gitignore` / `.ignore` files are respected by default. `require_git` is
/// disabled so gitignore-style filtering still applies when `root` is not
/// itself inside a formal git repository (e.g. a subdirectory scan, or a test
/// fixture) — matching the ergonomic expectation "a `.gitignore` file here
/// means what it says" rather than git's stricter repo-boundary rule.
pub fn scan_project(conn: &Connection, project: &str, root: &Path) -> Result<ScanStats> {
    let start = Instant::now();
    let mut stats = ScanStats::default();

    let mut builder = WalkBuilder::new(root);
    builder.require_git(false);
    builder.filter_entry(|entry| {
        if entry.depth() == 0 {
            return true;
        }
        entry
            .file_name()
            .to_str()
            .map(|name| !is_hard_skip_dir(name))
            .unwrap_or(true)
    });

    for entry in builder.build() {
        if stats.files_scanned >= MAX_FILES_PER_PROJECT {
            stats.truncated = true;
            break;
        }
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        if !has_code_extension(path) {
            continue;
        }

        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.len() > MAX_FILE_BYTES {
            stats.skipped_large += 1;
            continue;
        }

        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => continue, // binary / non-UTF8 — not a def source, skip silently
        };

        if looks_minified(path, &source) {
            stats.skipped_minified += 1;
            continue;
        }

        let stored_path = canonical_repo_path(path);
        let stored_path_str = stored_path.to_string_lossy().to_string();

        let fragment =
            extract_graph_fragment_for_file(&source, &stored_path_str, project, project, "", "");

        // Builtin/global name hygiene (TASK D): drop def rows that exactly
        // match a language builtin — see ScanStats::builtin_defs_skipped.
        let lang_key = manifest::builtin_lang_key_from_file(&stored_path_str);
        let mut defs: Vec<(String, String, String)> = Vec::new();
        for n in fragment.nodes.iter().filter(|n| n.kind != "module") {
            // synthetic file-anchor node, not a def
            if lang_key.is_some_and(|lk| manifest::classify_builtin(lk, &n.name)) {
                stats.builtin_defs_skipped += 1;
                continue;
            }
            defs.push((n.name.clone(), n.kind.clone(), n.lang.clone()));
        }

        stats.files_scanned += 1;
        stats.defs_indexed += defs.len();
        upsert_repo_defs(conn, project, &stored_path_str, &defs)?;
    }

    stats.duration_ms = start.elapsed().as_millis() as u64;
    Ok(stats)
}

/// Walk up from `file`'s parent directory looking for the nearest ancestor
/// containing `.git`, `Cargo.toml`, or `package.json`. At a given directory,
/// `.git` wins over a manifest found at that same level; otherwise the
/// nearest (shallowest walk-up) marker of any kind wins.
///
/// TASK D (WCR Phase 6) hygiene note: the walk hard-stops at (or above) the
/// user's home directory, even if a manifest file happens to sit there — a
/// stray `~/package.json` (verified in the wild: an accidental `npm init`
/// leftover) turned the home directory into an accepted "project root" for
/// any file whose real project had no closer marker, which made
/// `scan_project` walk the ENTIRE home tree (bounded only by
/// `MAX_FILES_PER_PROJECT`) — pulling in unrelated repos, Python virtualenvs,
/// and vendored `site-packages` as false `repo_defs` candidates. This is a
/// far larger source of "shadow noise" (spurious 2-file ambiguity for common
/// short names like `slice`/`stringify`) than any single vendored file could
/// be. A home directory is never a repository, so it's never accepted here,
/// full stop — conservative: `None` (no root) beats a wrong, oversized one.
fn nearest_root_marker(file: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    nearest_root_marker_within(file, home.as_deref())
}

/// Core of `nearest_root_marker`, with the home-directory boundary injected
/// rather than read from the environment — keeps the boundary check
/// unit-testable without mutating the real `$HOME` env var (unsafe to do
/// under a parallel test runner).
fn nearest_root_marker_within(file: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let mut dir = file.parent()?.to_path_buf();
    loop {
        if let Some(home) = home {
            if home == dir.as_path() || home.starts_with(&dir) {
                // `dir` IS the home directory, or is an ancestor of it
                // (e.g. `/Users`, `/`) — never a valid project root.
                return None;
            }
        }
        if dir.join(".git").exists()
            || dir.join("Cargo.toml").exists()
            || dir.join("package.json").exists()
        {
            return Some(dir);
        }
        match dir.parent() {
            Some(p) if p != dir.as_path() => dir = p.to_path_buf(),
            _ => return None,
        }
    }
}

/// Distinct project roots inferred from `code_nodes.file` values already
/// recorded for `project`: for each file, walk up to the nearest ancestor
/// with a repo/package marker, count occurrences per root, dedupe, keep only
/// roots that still exist on disk, and return at most 3 — largest
/// file-count first (ties broken by path for determinism).
pub fn project_roots(conn: &Connection, project: &str) -> Result<Vec<PathBuf>> {
    let mut stmt = conn.prepare("SELECT DISTINCT file FROM code_nodes WHERE project = ?1")?;
    let files: Vec<String> = stmt
        .query_map(params![project], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut counts: HashMap<PathBuf, usize> = HashMap::new();
    for file in &files {
        if file.is_empty() {
            continue;
        }
        // Defensive rewrite: some rows may predate the worktree-canonicalization
        // fix, or come from a writer that stored a raw path. Rewriting onto the
        // main-repo path here is what "dedupe after canonical_repo_path rewrite"
        // buys us — two worktree-relative variants of the same file collapse
        // onto the same root.
        let canon_file = canonical_repo_path(Path::new(file));
        if let Some(root) = nearest_root_marker(&canon_file) {
            if root.is_dir() {
                *counts.entry(root).or_insert(0) += 1;
            }
        }
    }

    let mut ranked: Vec<(PathBuf, usize)> = counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Ok(ranked.into_iter().take(3).map(|(p, _)| p).collect())
}

/// Resolve `project`'s roots (from previously recorded `code_nodes`) and scan
/// each, summing stats. A project with no recorded roots yet (e.g. before any
/// hook activity) simply yields `ScanStats::default()`.
pub fn scan_all(conn: &Connection, project: &str) -> Result<ScanStats> {
    let roots = project_roots(conn, project)?;
    let mut stats = ScanStats::default();
    for root in roots {
        let s = scan_project(conn, project, &root)?;
        stats.merge(&s);
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::codegraph::{upsert_node, NodeRow};
    use crate::storage::migrations;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        conn
    }

    #[test]
    fn scans_small_rust_file_and_indexes_defs() {
        let conn = mem();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn foo() {}\nstruct Bar {}\n").unwrap();

        let stats = scan_project(&conn, "proj", dir.path()).unwrap();
        assert_eq!(stats.files_scanned, 1, "stats: {stats:?}");
        assert_eq!(stats.defs_indexed, 2, "stats: {stats:?}");
        assert_eq!(stats.skipped_large, 0);
        assert!(!stats.truncated);

        let foo_hits = crate::storage::codegraph::lookup_repo_defs(&conn, "proj", "foo").unwrap();
        assert_eq!(foo_hits.len(), 1, "foo hits: {foo_hits:?}");
        assert_eq!(foo_hits[0].1, "function");

        let bar_hits = crate::storage::codegraph::lookup_repo_defs(&conn, "proj", "Bar").unwrap();
        assert_eq!(bar_hits.len(), 1, "bar hits: {bar_hits:?}");
        assert_eq!(bar_hits[0].1, "type");
    }

    #[test]
    fn oversized_file_is_skipped_and_counted() {
        let conn = mem();
        let dir = tempfile::tempdir().unwrap();
        // Content doesn't need to parse — size cap trips before extraction.
        let filler = "x".repeat((MAX_FILE_BYTES as usize) + 1024);
        std::fs::write(dir.path().join("big.rs"), filler).unwrap();

        let stats = scan_project(&conn, "proj", dir.path()).unwrap();
        assert_eq!(stats.files_scanned, 0, "stats: {stats:?}");
        assert_eq!(stats.skipped_large, 1, "stats: {stats:?}");
        assert_eq!(stats.defs_indexed, 0);
    }

    #[test]
    fn gitignored_file_is_not_scanned() {
        let conn = mem();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.rs"), "fn kept() {}\n").unwrap();
        std::fs::write(dir.path().join("ignored.rs"), "fn ghost() {}\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.rs\n").unwrap();

        let stats = scan_project(&conn, "proj", dir.path()).unwrap();
        // Only keep.rs must be scanned: 1 file, 1 def. `require_git(false)` is
        // what makes this honest — the ignore crate only honors .gitignore
        // outside a real `.git` repo when that flag is set; without it this
        // tempdir fixture (no `.git` present) would NOT be gitignore-filtered
        // and this assertion would fail, catching a config regression.
        assert_eq!(stats.files_scanned, 1, "stats: {stats:?}");
        assert_eq!(stats.defs_indexed, 1, "stats: {stats:?}");

        let ghost_hits =
            crate::storage::codegraph::lookup_repo_defs(&conn, "proj", "ghost").unwrap();
        assert!(ghost_hits.is_empty(), "ghost hits: {ghost_hits:?}");
        let kept_hits = crate::storage::codegraph::lookup_repo_defs(&conn, "proj", "kept").unwrap();
        assert_eq!(kept_hits.len(), 1, "kept hits: {kept_hits:?}");
    }

    // ─── Repo_defs hygiene (WCR Phase 6, TASK D) ───

    #[test]
    fn scan_skips_def_rows_matching_language_builtins() {
        let conn = mem();
        let dir = tempfile::tempdir().unwrap();
        // `fetch` is a JS_GLOBAL (manifest::classify_builtin) — a vendored
        // file re-exporting it is shadow noise, not a real user symbol.
        std::fs::write(
            dir.path().join("a.js"),
            "export function fetch() {}\nexport function realHelper() {}\n",
        )
        .unwrap();

        let stats = scan_project(&conn, "proj", dir.path()).unwrap();
        assert_eq!(stats.files_scanned, 1, "stats: {stats:?}");
        assert_eq!(
            stats.builtin_defs_skipped, 1,
            "fetch must be counted as skipped: {stats:?}"
        );
        assert_eq!(stats.defs_indexed, 1, "only realHelper indexed: {stats:?}");

        let fetch_hits =
            crate::storage::codegraph::lookup_repo_defs(&conn, "proj", "fetch").unwrap();
        assert!(
            fetch_hits.is_empty(),
            "builtin-named def must not land in repo_defs: {fetch_hits:?}"
        );
        let helper_hits =
            crate::storage::codegraph::lookup_repo_defs(&conn, "proj", "realHelper").unwrap();
        assert_eq!(helper_hits.len(), 1, "helper hits: {helper_hits:?}");
    }

    #[test]
    fn scan_skips_minified_files_by_extension() {
        let conn = mem();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("vendor.min.js"),
            "function realFunctionName(){return 1}\n",
        )
        .unwrap();

        let stats = scan_project(&conn, "proj", dir.path()).unwrap();
        assert_eq!(stats.files_scanned, 0, "stats: {stats:?}");
        assert_eq!(stats.skipped_minified, 1, "stats: {stats:?}");
        assert_eq!(stats.defs_indexed, 0);
    }

    #[test]
    fn scan_skips_files_with_a_single_extremely_long_line() {
        let conn = mem();
        let dir = tempfile::tempdir().unwrap();
        // Not `.min.js`, but a single line far past the threshold — the
        // hallmark of a minified/bundled build output checked in under a
        // plain `.js` name.
        let long_line = format!("function bundled(){{{}return 1}}\n", "/*pad*/".repeat(400));
        assert!(long_line.lines().next().unwrap().len() > MINIFIED_LINE_LEN_THRESHOLD);
        std::fs::write(dir.path().join("bundle.js"), &long_line).unwrap();

        let stats = scan_project(&conn, "proj", dir.path()).unwrap();
        assert_eq!(stats.files_scanned, 0, "stats: {stats:?}");
        assert_eq!(stats.skipped_minified, 1, "stats: {stats:?}");
    }

    #[test]
    fn normal_file_is_not_treated_as_minified() {
        let conn = mem();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("normal.rs"), "fn normal_fn() {}\n").unwrap();

        let stats = scan_project(&conn, "proj", dir.path()).unwrap();
        assert_eq!(stats.files_scanned, 1, "stats: {stats:?}");
        assert_eq!(stats.skipped_minified, 0, "stats: {stats:?}");
        assert_eq!(stats.defs_indexed, 1);
    }

    #[test]
    fn nearest_root_marker_never_accepts_the_home_directory_as_a_root() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let deep = home.join("projects/some_repo/src");
        std::fs::create_dir_all(&deep).unwrap();
        // A stray package.json sitting directly in $HOME (verified in the
        // wild) — must never be accepted as a project root. The deep file's
        // real project has no manifest of its own closer than $HOME, so the
        // walk must yield None rather than degrading into a home-wide scan.
        std::fs::write(home.join("package.json"), "{}").unwrap();
        let file = deep.join("orphan.ts");

        let result = nearest_root_marker_within(&file, Some(&home));
        assert!(
            result.is_none(),
            "home directory must never be accepted as a root: {result:?}"
        );
    }

    #[test]
    fn nearest_root_marker_stops_at_ancestors_of_home_too() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("users/alice");
        let deep = home.join("scratch/orphan");
        std::fs::create_dir_all(&deep).unwrap();
        // No manifest anywhere on this path at all — asserts the walk
        // doesn't escape past an ancestor-of-home boundary either.
        let file = deep.join("x.ts");

        let result = nearest_root_marker_within(&file, Some(&home));
        assert!(
            result.is_none(),
            "must stop, not walk past home: {result:?}"
        );
    }

    #[test]
    fn nearest_root_marker_still_finds_a_real_manifest_below_home() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let repo = home.join("projects/real_repo");
        let deep = repo.join("src");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(repo.join("package.json"), "{}").unwrap();
        let file = deep.join("a.ts");

        let result = nearest_root_marker_within(&file, Some(&home));
        assert_eq!(
            result,
            Some(repo),
            "a real manifest below home must still be found"
        );
    }

    #[test]
    fn project_roots_finds_nearest_manifest_ancestor() {
        let conn = mem();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj_root");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let file = src.join("lib.rs");
        std::fs::write(&file, "fn a() {}\n").unwrap();

        upsert_node(
            &conn,
            &NodeRow {
                id: "n1".into(),
                project: "proj".into(),
                file: file.to_string_lossy().to_string(),
                kind: "function".into(),
                name: "a".into(),
                ..NodeRow::default()
            },
        )
        .unwrap();

        let roots = project_roots(&conn, "proj").unwrap();
        assert_eq!(roots.len(), 1, "roots: {roots:?}");
        let expected = std::fs::canonicalize(&root).unwrap_or(root);
        let got = std::fs::canonicalize(&roots[0]).unwrap_or_else(|_| roots[0].clone());
        assert_eq!(got, expected, "roots: {roots:?}");
    }

    #[test]
    fn project_roots_caps_at_three_largest_first() {
        let conn = mem();
        let dir = tempfile::tempdir().unwrap();

        let mut expected_top: Option<PathBuf> = None;
        for i in 0..4 {
            let root = dir.path().join(format!("root{i}"));
            let src = root.join("src");
            std::fs::create_dir_all(&src).unwrap();
            std::fs::write(root.join("package.json"), "{}").unwrap();
            // root0 gets 3 files (the winner), the rest get 1 each.
            let n = if i == 0 { 3 } else { 1 };
            for j in 0..n {
                let file = src.join(format!("f{j}.ts"));
                std::fs::write(&file, "export function a() {}\n").unwrap();
                upsert_node(
                    &conn,
                    &NodeRow {
                        id: format!("n{i}_{j}"),
                        project: "proj".into(),
                        file: file.to_string_lossy().to_string(),
                        kind: "function".into(),
                        name: "a".into(),
                        ..NodeRow::default()
                    },
                )
                .unwrap();
            }
            if i == 0 {
                expected_top = Some(std::fs::canonicalize(&root).unwrap_or(root));
            }
        }

        let roots = project_roots(&conn, "proj").unwrap();
        assert_eq!(roots.len(), 3, "roots: {roots:?}");
        let got_top = std::fs::canonicalize(&roots[0]).unwrap_or_else(|_| roots[0].clone());
        assert_eq!(got_top, expected_top.unwrap(), "roots: {roots:?}");
    }

    #[test]
    fn scan_all_with_no_recorded_roots_is_a_noop() {
        let conn = mem();
        let stats = scan_all(&conn, "empty-proj").unwrap();
        assert_eq!(stats, ScanStats::default());
    }

    /// Not part of the required test matrix — an optional, cheap self-bench
    /// measuring scan duration on this crate's own `src/`. Run explicitly:
    /// `cargo test --lib extraction::repo_scan::tests::self_bench_scan_duration -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn self_bench_scan_duration() {
        let conn = mem();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let stats = scan_project(&conn, "csr-engine-self", &root).unwrap();
        eprintln!(
            "self-bench: {} files, {} defs, {}ms (truncated={})",
            stats.files_scanned, stats.defs_indexed, stats.duration_ms, stats.truncated
        );
        assert!(stats.files_scanned > 0, "stats: {stats:?}");
    }
}
