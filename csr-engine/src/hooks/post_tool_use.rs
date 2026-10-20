//! PostToolUse hook — imports transcript + tracks code evolution.
//!
//! Fires after each successful tool call. Imports the current transcript
//! so new content becomes searchable. For Edit/Write operations, tracks
//! structural code changes (functions/types/imports added/removed).
//!
//! A Bash call is read first (`hooks::bash_writes`): one that wrote no file
//! returns before the engine is built, so the most frequent tool costs a
//! process start and a parse. One that wrote files takes the same path an Edit
//! takes, once per file, with `tool_name = 'Bash'` and no symbol diff (a shell
//! command carries no before/after text, and a diff is never invented).
//!
//! Always returns Ok(()) — never blocks Claude Code (C-2 fix).

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::HookInput;
use crate::engine::Engine;

/// Tool names whose edits this hook tracks (code evolution + code graph).
/// The transcript sync above the gate runs for every fire regardless; the
/// installer's PostToolUse matcher (`Edit|Write|MultiEdit|NotebookEdit|Bash`,
/// see `hooks::install`) is deliberately a superset of this list. `Bash` is
/// not here: it names no file in `tool_input`, so it has its own path through
/// [`bash_written_files`].
pub fn is_acted_on_tool(tool_name: Option<&str>) -> bool {
    matches!(tool_name, Some("Edit") | Some("Write") | Some("MultiEdit"))
}

/// Most files one Bash call may add to the file history. A script that rewrites
/// a whole tree would otherwise make one hook run parse and store all of it.
const MAX_BASH_FILES: usize = 16;

/// The existing files a Bash call wrote, by reading its command. Empty for any
/// other tool and for a Bash call that wrote nothing. Paths resolve against the
/// cwd Claude Code sent, else the process cwd.
pub fn bash_written_files(input: &HookInput) -> Vec<PathBuf> {
    if input.tool_name.as_deref() != Some("Bash") {
        return Vec::new();
    }
    let Some(command) = input
        .tool_input
        .as_ref()
        .and_then(|t| t.get("command"))
        .and_then(|v| v.as_str())
    else {
        return Vec::new();
    };
    let cwd = match input.cwd.as_deref().filter(|c| !c.trim().is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir().unwrap_or_default(),
    };
    let mut files = super::bash_writes::bash_writes(command, &cwd);
    files.retain(|f| f.is_file());
    files.truncate(MAX_BASH_FILES);
    files
}

/// Whether this hook run has nothing to do and can exit before the engine is
/// built: a Bash PostToolUse that wrote no file. Everything else, including a
/// Bash call that did write, goes on to [`handle`].
pub fn exits_before_engine(hook_name: &str, input: &HookInput) -> bool {
    hook_name == "post-tool-use"
        && input.tool_name.as_deref() == Some("Bash")
        && bash_written_files(input).is_empty()
}

/// Handle the post-tool-use hook.
/// Always returns Ok(()) to never block Claude Code (C-2 fix).
pub async fn handle(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    let bash_files = bash_written_files(input);
    if input.tool_name.as_deref() == Some("Bash") && bash_files.is_empty() {
        return Ok(());
    }
    // Code map session signal: this cwd has a live session. One upsert, first,
    // so the slow transcript import below cannot delay or starve it.
    super::record_codemap_signal(input, engine, "post_tool_use");
    // A session in a protected folder (Desktop, Documents, ...) is mapped by the
    // session itself: queue the edit and start the detached drain. No-op elsewhere.
    super::codemap_session_post_tool_use(input, engine);

    // Import growing transcript (real-time searchability)
    super::import_current_transcript(input, engine, cwd).await;

    // Track code evolution for Edit/Write/MultiEdit operations (v9) and for
    // the files a Bash call wrote.
    let touched: Vec<String> = if is_acted_on_tool(input.tool_name.as_deref()) {
        tool_file_path(input)
            .map(str::to_string)
            .into_iter()
            .collect()
    } else {
        bash_files
            .iter()
            .map(|f| f.to_string_lossy().into_owned())
            .collect()
    };
    for file_path in &touched {
        // Code map freshness: enqueue only, the daemon does the work. One
        // INSERT, no parse, no git; a failure (missing table, locked db) is
        // logged and never reaches Claude Code.
        if let Err(e) = enqueue_path_for_codemap(engine, file_path) {
            eprintln!("CSR: code map enqueue error (non-fatal): {}", e);
        }
        if let Err(e) =
            track_code_evolution_for(input, engine, file_path, resolve_project_for_hook).await
        {
            eprintln!("CSR: code evolution tracking error (non-fatal): {}", e);
        }
        // v9.4 liveness path: re-extract the touched file into the code graph
        // so callers/callees/ledger reflect the edit immediately.
        if let Err(e) = update_code_graph_for(input, engine, file_path, resolve_project_for_hook) {
            eprintln!("CSR: code graph update error (non-fatal): {}", e);
        }
    }

    Ok(())
}

/// The `file_path` an Edit/Write/MultiEdit sent.
fn tool_file_path(input: &HookInput) -> Option<&str> {
    input
        .tool_input
        .as_ref()
        .and_then(|t| t.get("file_path"))
        .and_then(|v| v.as_str())
}

/// Queue the edited file for the code map's partial rescan. Skipped when the
/// code map is off, the path is relative, not a code file, or protected.
#[cfg(test)]
fn enqueue_for_codemap(input: &HookInput, engine: &Engine) -> Result<()> {
    match tool_file_path(input) {
        Some(file_path) => enqueue_path_for_codemap(engine, file_path),
        None => Ok(()),
    }
}

fn enqueue_path_for_codemap(engine: &Engine, file_path: &str) -> Result<()> {
    let path = Path::new(file_path);
    if !crate::codemap::should_enqueue(path, crate::codemap::identity::home_dir().as_deref()) {
        return Ok(());
    }
    engine
        .storage()
        .with_connection(|conn| enqueue_best_effort(conn, path))
}

/// One queue INSERT under a short busy timeout (see
/// `codemap::with_hook_busy_timeout`): the enqueue is best effort, so contention
/// means skip.
fn enqueue_best_effort(conn: &rusqlite::Connection, path: &Path) -> Result<()> {
    crate::codemap::with_hook_busy_timeout(conn, || crate::codemap::enqueue(conn, path))
}

/// Conversation id for provenance: transcript filename stem, else session id.
fn conv_id_for(input: &HookInput) -> String {
    if let Some(ref tp) = input.transcript_path {
        if let Some(stem) = std::path::Path::new(tp).file_stem() {
            let s = stem.to_string_lossy().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    input.session_id.clone().unwrap_or_default()
}

/// Maximum file size we will re-parse into the code graph (256KB).
const MAX_GRAPH_FILE_BYTES: u64 = 256 * 1024;

/// Re-extract the edited file into the code graph (v9.4 liveness path).
/// Reads the on-disk file (post-edit), upserts nodes, replaces this file's edges,
/// marks file state, then re-resolves + re-ranks the project.
#[cfg(test)]
fn update_code_graph(input: &HookInput, engine: &Engine) -> Result<()> {
    update_code_graph_with(input, engine, resolve_project_for_hook)
}

/// `update_code_graph` with the project resolver passed in, so tests drive it
/// without mutating process-global env vars. `resolve` takes the edited file's
/// parent and the hook input's session cwd.
#[cfg(test)]
fn update_code_graph_with(
    input: &HookInput,
    engine: &Engine,
    resolve: impl Fn(Option<&Path>, Option<&str>) -> String,
) -> Result<()> {
    let file_path =
        tool_file_path(input).ok_or_else(|| anyhow::anyhow!("no file_path in tool_input"))?;
    update_code_graph_for(input, engine, file_path, resolve)
}

/// Core of `update_code_graph` for one file: an Edit's `file_path` or one of
/// the files a Bash call wrote.
fn update_code_graph_for(
    input: &HookInput,
    engine: &Engine,
    file_path: &str,
    resolve: impl Fn(Option<&Path>, Option<&str>) -> String,
) -> Result<()> {
    // Repo/project identity is derivable from `file_path` alone (no lang
    // needed), so compute it before the language gate — WP2 Stage 3 (H8
    // innovation, receipt R4) wants an honest file-level row even for
    // extensions the extractor doesn't parse, instead of the file vanishing
    // silently.
    let project = resolve(Path::new(file_path).parent(), input.cwd.as_deref());
    // No project, no graph write. `resolve_code_edges("")` and
    // `compute_code_rank("")` read `''` as EVERY project (`?1 = '' OR project
    // = ?1`), so a scratch file with no session project would bind a call in
    // one project to a definition in another and re-rank the whole graph from
    // inside the hook. A node filed under `''` is no better: no scoped query
    // would ever find it.
    if project.is_empty() {
        return Ok(());
    }
    let stored_path = crate::extraction::repo_path::canonical_repo_path(Path::new(file_path));
    let stored_path_str = stored_path.to_string_lossy();

    let lang = match crate::extraction::ast_analysis::lang_from_path_str(file_path) {
        Some(l) => l,
        None => {
            // Unsupported language — record file-level provenance instead of
            // dropping the file entirely (best-effort; never blocks the hook).
            if let Err(e) = engine
                .storage()
                .mark_code_file_unsupported(&project, &stored_path_str)
            {
                eprintln!("CSR: ast_status=unsupported record error (non-fatal): {e}");
            }
            return Ok(());
        }
    };

    let meta = std::fs::metadata(file_path);
    if let Ok(m) = &meta {
        if m.len() > MAX_GRAPH_FILE_BYTES {
            return Ok(());
        }
    }
    // Read from the original path (worktree-local); store under the canonical
    // main-repo path so worktree edits do not create duplicate nodes.
    let source = match std::fs::read_to_string(file_path) {
        Ok(s) => s,
        Err(_) => return Ok(()), // file gone/unreadable — skip silently
    };

    let session_id = input.session_id.clone().unwrap_or_default();
    let conv_id = conv_id_for(input);

    let fragment = crate::extraction::codegraph::extract_graph_fragment(
        &source,
        lang,
        &stored_path_str,
        &project,
        &project,
        &conv_id,
        &session_id,
    );

    // Repo identity (WP2 Stage 1, H8 finding): stable across cwd/session
    // boundaries, unlike `project` — never overwrites it.
    let repo_root = crate::extraction::repo_root::repo_root_for_file(&stored_path_str);

    let storage = engine.storage();
    let source_chunk_id = storage.latest_chunk_id_for_conversation(&conv_id)?;
    for node in &fragment.nodes {
        let mut node = node.clone();
        node.repo_root = repo_root.clone();
        let changed = storage
            .get_code_node(&node.id)?
            .is_none_or(|stored| stored.body_hash != node.body_hash);
        storage.upsert_code_node(&node)?;
        if changed {
            if let Some(chunk_id) = source_chunk_id.as_deref() {
                storage.set_code_node_last_chunk(&node.id, chunk_id)?;
            }
        }
    }
    let seen_node_ids: Vec<String> = fragment.nodes.iter().map(|n| n.id.clone()).collect();
    // Only a CLEAN parse may drive the destructive half. A partial parse still
    // returns some valid definitions — `extract_graph_fragment` sets
    // `parse_clean = false` while `nodes` stays non-empty — and treating that
    // truncated view as authoritative would retire every symbol it failed to
    // see, taking each one's `code_node_attribution` provenance with it. Files
    // are routinely observed mid-edit, so this is a common path, not a corner
    // case. Upserts above are additive and safe unconditionally; only
    // retirement and edge replacement are gated. This is the same signal
    // `eval::codegraph` and `import::backfill` already gate on.
    if fragment.parse_clean {
        storage.retire_missing_code_nodes(&project, &stored_path_str, &seen_node_ids)?;
        storage.replace_code_file_edges(&project, &stored_path_str, &fragment.edges)?;
    }

    let content_hash = crate::extraction::anchors::hash_normalized(&source);
    storage.upsert_code_file_state(&project, &stored_path_str, &content_hash, false)?;

    // Re-resolve + re-rank so the live graph is queryable right after the edit.
    let _ = storage.resolve_code_edges(&project)?;
    let _ = storage.compute_code_rank(&project)?;

    eprintln!(
        "CSR: code graph updated for {} ({} nodes, {} edges)",
        stored_path_str,
        fragment.nodes.len(),
        fragment.edges.len()
    );

    Ok(())
}

/// Paths a file-level presence row must never record: credentials, dependency
/// trees and build output. High volume, no provenance value, and for the
/// credential cases the stored path is itself a disclosure. Only consulted for
/// the unsupported-extension "touch row" branch — the AST path is already
/// bounded by the language allowlist.
pub(crate) fn is_unrecordable_path(path: &str) -> bool {
    const NOISE_DIRS: [&str; 9] = [
        "/node_modules/",
        "/target/",
        "/.git/",
        "/dist/",
        "/build/",
        "/vendor/",
        "/.venv/",
        "/__pycache__/",
        "/.next/",
    ];
    if NOISE_DIRS.iter().any(|seg| path.contains(seg)) {
        return true;
    }
    let name = path.rsplit('/').next().unwrap_or("");
    name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.ends_with(".p12")
}

/// Detect programming language from file extension.
///
/// Shared with `import::coedit_backfill` so historically replayed rows use
/// the exact same language taxonomy as the live hook.
pub(crate) fn detect_language(file_path: &str) -> &'static str {
    let ext = file_path.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => "rust",
        "py" => "python",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "go" => "go",
        _ => "",
    }
}

/// Maximum bytes for AST diffing (50KB) — skip large generated files.
const MAX_AST_DIFF_BYTES: usize = 50_000;

/// Track structural code changes from Edit/Write operations.
#[cfg(test)]
async fn track_code_evolution(input: &HookInput, engine: &Engine) -> Result<()> {
    track_code_evolution_with(input, engine, resolve_project_for_hook).await
}

/// `track_code_evolution` with the project resolver passed in (see
/// `update_code_graph_with`).
#[cfg(test)]
async fn track_code_evolution_with(
    input: &HookInput,
    engine: &Engine,
    resolve: impl Fn(Option<&Path>, Option<&str>) -> String,
) -> Result<()> {
    if input.tool_input.is_none() {
        return Err(anyhow::anyhow!("no tool_input"));
    }
    let file_path =
        tool_file_path(input).ok_or_else(|| anyhow::anyhow!("no file_path in tool_input"))?;
    track_code_evolution_for(input, engine, file_path, resolve).await
}

/// Core of `track_code_evolution` for one file: an Edit's `file_path` or one of
/// the files a Bash call wrote. A Bash row always lands (the call did write the
/// file) with empty symbol lists: the command carries no before/after text.
async fn track_code_evolution_for(
    input: &HookInput,
    engine: &Engine,
    file_path: &str,
    resolve: impl Fn(Option<&Path>, Option<&str>) -> String,
) -> Result<()> {
    let tool_input = input
        .tool_input
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no tool_input"))?;

    let language = detect_language(file_path);
    let tool_name = input.tool_name.as_deref().unwrap_or("unknown");
    let is_bash = tool_name == "Bash";

    // Mirrors the code-graph policy at update_code_graph (lines 68–88):
    // record an honest file-level presence row instead of silently dropping.
    if language.is_empty() {
        let conv_id = conv_id_for(input);
        let project_name = resolve(Path::new(file_path).parent(), input.cwd.as_deref());
        // A scratch file with no session project is not project history.
        if project_name.is_empty() {
            return Ok(());
        }
        let stored_path = crate::extraction::repo_path::canonical_repo_path(Path::new(file_path));
        let stored_path_str = stored_path.to_string_lossy();
        let repo_root = crate::extraction::repo_root::repo_root_for_file(&stored_path_str);
        // Bound what a blanket presence row may record. This branch fires for
        // EVERY unsupported extension, so without a boundary every scratch
        // file, secret and vendored artifact a session touches becomes a
        // permanent timeline entry — and for a secret the stored path is itself
        // the disclosure. A file outside any repository is not project history.
        if repo_root.is_none() || is_unrecordable_path(&stored_path_str) {
            return Ok(());
        }
        engine.storage().insert_code_evolution(
            &conv_id,
            &project_name,
            &stored_path_str,
            "text",
            tool_name,
            "[]",
            "[]",
            "[]",
            "[]",
            "[]",
            "[]",
            repo_root.as_deref(),
        )?;
        return Ok(());
    }

    // Build before/after pairs depending on tool type (Codex M-2: handle MultiEdit edits array)
    let edit_pairs: Vec<(String, String)> = if is_bash {
        // No before/after text: nothing to diff, and none is invented.
        Vec::new()
    } else if tool_name == "Edit" {
        let old = tool_input
            .get("old_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let new = tool_input
            .get("new_string")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        vec![(old.to_string(), new.to_string())]
    } else if tool_name == "MultiEdit" {
        // MultiEdit uses an edits array of {old_string, new_string} pairs
        tool_input
            .get("edits")
            .and_then(|v| v.as_array())
            .map(|edits| {
                edits
                    .iter()
                    .map(|e| {
                        let old = e.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
                        let new = e.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
                        (old.to_string(), new.to_string())
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        // Write: everything is new
        let content = tool_input
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        vec![(String::new(), content.to_string())]
    };

    // Merge AST diffs from all edit pairs
    let mut diff = crate::extraction::ast_analysis::AstDiff::default();
    for (before, after) in &edit_pairs {
        if before.len() > MAX_AST_DIFF_BYTES || after.len() > MAX_AST_DIFF_BYTES {
            continue;
        }
        let d = crate::extraction::ast_analysis::compute_ast_diff(before, after, language);
        diff.functions_added.extend(d.functions_added);
        diff.functions_removed.extend(d.functions_removed);
        diff.types_added.extend(d.types_added);
        diff.types_removed.extend(d.types_removed);
        diff.imports_added.extend(d.imports_added);
        diff.imports_removed.extend(d.imports_removed);
    }

    // Only store if something structural changed; a Bash write is stored
    // regardless, as the record that this session wrote the file.
    if diff.is_empty() && !is_bash {
        return Ok(());
    }

    // As of this fix: this column holds conversation ids (transcript stem), not raw session ids —
    // see conv_id_for. Needed so the reinstatement graph walk (which joins on conversation_id)
    // finds code evolution rows for sidechain/resumed sessions too.
    let conv_id = conv_id_for(input);

    // Resolve project name for cross-project scoping
    let project_name = resolve(Path::new(file_path).parent(), input.cwd.as_deref());
    // A scratch file with no session project is not project history.
    if project_name.is_empty() {
        return Ok(());
    }

    // Read from the original path (worktree-local); store under the canonical
    // main-repo path so worktree edits do not create duplicate rows (D5 — mirrors
    // `update_code_graph` and `import::coedit_backfill`, the two sibling write
    // paths that already canonicalize before storing).
    let stored_path = crate::extraction::repo_path::canonical_repo_path(Path::new(file_path));
    let stored_path_str = stored_path.to_string_lossy();
    // A shell redirect reaches paths an edit tool never names (`> node_modules/x.js`).
    if is_bash && is_unrecordable_path(&stored_path_str) {
        return Ok(());
    }

    // Serialize to JSON arrays
    let fa = serde_json::to_string(&diff.functions_added).unwrap_or_default();
    let fr = serde_json::to_string(&diff.functions_removed).unwrap_or_default();
    let ta = serde_json::to_string(&diff.types_added).unwrap_or_default();
    let tr = serde_json::to_string(&diff.types_removed).unwrap_or_default();
    let ia = serde_json::to_string(&diff.imports_added).unwrap_or_default();
    let ir = serde_json::to_string(&diff.imports_removed).unwrap_or_default();

    // Repo identity (WP2 Stage 1, H8 finding): stable across cwd/session
    // boundaries, unlike `project_name` — never overwrites it. Resolved from the
    // canonical path (same as `update_code_graph`): `git rev-parse --show-toplevel`
    // run against a worktree-local path returns the worktree's own root, not the
    // main repo's — resolving from the raw path would produce a wrong repo_root
    // even after the file_path fix above.
    let repo_root = crate::extraction::repo_root::repo_root_for_file(&stored_path_str);

    engine.storage().insert_code_evolution(
        &conv_id,
        &project_name,
        &stored_path_str,
        language,
        tool_name,
        &fa,
        &fr,
        &ta,
        &tr,
        &ia,
        &ir,
        repo_root.as_deref(),
    )?;

    eprintln!(
        "CSR: tracked code evolution for {} (+{} fns, -{} fns)",
        stored_path_str,
        diff.functions_added.len(),
        diff.functions_removed.len()
    );

    Ok(())
}

/// Resolve project name for hook processes (no `MCP_CLIENT_CWD`).
///
/// Fallback chain:
/// 1. parent directory of the edited file (via `resolve_project_from_cwd`),
///    skipped for a temp path: a scratch file's folder name ("scratchpad",
///    "bin") is not a project, so it takes the session's project instead
/// 2. `CLAUDE_PROJECT_DIR` env var
/// 3. the hook input's own session `cwd`, when non-blank
/// 4. `MCP_CLIENT_CWD` via `resolve_current_project` (MCP tool path)
/// 5. empty string, which callers treat as "no project" and write nothing for
///    (`''` means every project in the resolver and ranker SQL)
fn resolve_project_for_hook(file_path_parent: Option<&Path>, session_cwd: Option<&str>) -> String {
    resolve_project_for_hook_with(
        file_path_parent,
        std::env::var("CLAUDE_PROJECT_DIR").ok(),
        session_cwd,
        crate::search::cross_project::resolve_current_project(),
    )
}

/// Env-free core of `resolve_project_for_hook`: fallback values are passed
/// in so tests never mutate process-global env vars (CodeRabbit PR #279 —
/// `remove_var` in one test races other tests reading the same vars).
fn resolve_project_for_hook_with(
    file_path_parent: Option<&Path>,
    claude_project_dir: Option<String>,
    session_cwd: Option<&str>,
    current_project: Option<String>,
) -> String {
    if let Some(parent) = file_path_parent.filter(|p| !crate::codemap::identity::is_temp_path(p)) {
        let parent_str = parent.to_string_lossy();
        if let Some(p) = crate::search::cross_project::resolve_project_from_cwd(&parent_str) {
            if !p.is_empty() {
                return p;
            }
        }
    }
    if let Some(dir) = claude_project_dir {
        if let Some(p) = crate::search::cross_project::resolve_project_from_cwd(&dir) {
            if !p.is_empty() {
                return p;
            }
        }
    }
    if let Some(cwd) = session_cwd.filter(|c| !c.trim().is_empty()) {
        if let Some(p) = crate::search::cross_project::resolve_project_from_cwd(cwd) {
            if !p.is_empty() {
                return p;
            }
        }
    }
    if let Some(p) = current_project {
        if !p.is_empty() {
            return p;
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_acted_on_tool_true_for_edit_write_multiedit() {
        assert!(is_acted_on_tool(Some("Edit")));
        assert!(is_acted_on_tool(Some("Write")));
        assert!(is_acted_on_tool(Some("MultiEdit")));
    }

    #[test]
    fn is_acted_on_tool_false_for_everything_else() {
        assert!(!is_acted_on_tool(Some("Read")));
        assert!(!is_acted_on_tool(Some("Bash")));
        assert!(!is_acted_on_tool(Some("Grep")));
        assert!(!is_acted_on_tool(None));
    }

    #[test]
    fn resolve_project_for_hook_from_file_parent() {
        // Env-free variant — passing None for both fallbacks isolates the
        // parent-directory chain without mutating process-global env vars.
        let parent = Path::new("/Users/x/projects/my-repo/src/main.rs").parent();
        let project = resolve_project_for_hook_with(parent, None, None, None);
        assert_eq!(project, "my-repo");
        assert!(!project.is_empty());
    }

    #[test]
    fn resolve_project_for_hook_gives_a_temp_file_the_session_project() {
        let scratchpad =
            Path::new("/private/tmp/claude-501/-Users-x-projects-my-repo/sid/scratchpad/fix.py");
        let session = Some("/Users/x/projects/my-repo".to_string());
        assert_eq!(
            resolve_project_for_hook_with(scratchpad.parent(), session, None, None),
            "my-repo"
        );
        let patcher = Path::new("/tmp/patches/bin/apply.py");
        assert_eq!(
            resolve_project_for_hook_with(patcher.parent(), None, None, Some("my-repo".into())),
            "my-repo"
        );
        // No session context: an empty project, never the folder name.
        assert_eq!(
            resolve_project_for_hook_with(patcher.parent(), None, None, None),
            ""
        );
        // A folder merely named tmp is not a temp root.
        let repo_file = Path::new("/Users/x/projects/my-repo/tmp/a.rs");
        assert_eq!(
            resolve_project_for_hook_with(repo_file.parent(), None, None, None),
            "my-repo"
        );
    }

    #[test]
    fn resolve_project_for_hook_falls_back_to_the_hook_input_cwd() {
        let patcher = Path::new("/tmp/patches/bin/apply.py");
        // The hook's own session cwd names the project when the file and
        // CLAUDE_PROJECT_DIR do not.
        assert_eq!(
            resolve_project_for_hook_with(
                patcher.parent(),
                None,
                Some("/Users/x/projects/my-repo"),
                None
            ),
            "my-repo"
        );
        // Order: CLAUDE_PROJECT_DIR beats the session cwd, the session cwd
        // beats MCP_CLIENT_CWD.
        assert_eq!(
            resolve_project_for_hook_with(
                patcher.parent(),
                Some("/Users/x/projects/from-env".into()),
                Some("/Users/x/projects/my-repo"),
                None
            ),
            "from-env"
        );
        assert_eq!(
            resolve_project_for_hook_with(
                patcher.parent(),
                None,
                Some("/Users/x/projects/my-repo"),
                Some("from-mcp".into())
            ),
            "my-repo"
        );
        // A blank cwd is no cwd: it falls through to the next step, then to "".
        for blank in ["", "   "] {
            assert_eq!(
                resolve_project_for_hook_with(patcher.parent(), None, Some(blank), None),
                ""
            );
            assert_eq!(
                resolve_project_for_hook_with(
                    patcher.parent(),
                    None,
                    Some(blank),
                    Some("from-mcp".into())
                ),
                "from-mcp"
            );
        }
        // A real repo file keeps its own project over the session cwd.
        let repo_file = Path::new("/Users/x/projects/my-repo/src/a.rs");
        assert_eq!(
            resolve_project_for_hook_with(
                repo_file.parent(),
                None,
                Some("/Users/x/projects/other"),
                None
            ),
            "my-repo"
        );
    }

    /// Row counts of every table a hook edit can write the code graph into.
    fn graph_rows(storage: &crate::storage::Storage) -> Vec<(String, i64)> {
        [
            "code_nodes",
            "code_edges",
            "code_graph_file_state",
            "code_evolution",
        ]
        .iter()
        .map(|t| {
            let n: i64 = storage
                .with_connection(|c| {
                    Ok(c.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))?)
                })
                .unwrap();
            (t.to_string(), n)
        })
        .collect()
    }

    /// Env-free resolver for the tests below: no CLAUDE_PROJECT_DIR, no
    /// MCP_CLIENT_CWD, only the file and the hook input's own cwd.
    fn resolve_without_env(parent: Option<&Path>, cwd: Option<&str>) -> String {
        resolve_project_for_hook_with(parent, None, cwd, None)
    }

    fn write_input(file: &Path, cwd: Option<&str>) -> crate::hooks::HookInput {
        crate::hooks::HookInput {
            transcript_path: Some("/tmp/nonexistent-test-transcript.jsonl".to_string()),
            session_id: Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into()),
            cwd: cwd.map(str::to_string),
            tool_name: Some("Write".into()),
            tool_input: Some(serde_json::json!({
                "file_path": file.to_string_lossy().to_string(),
                "content": "fn scratch_fn() { helper(); }\nfn helper() {}\n",
            })),
            ..Default::default()
        }
    }

    #[test]
    fn a_scratch_file_with_no_project_leaves_the_code_graph_untouched() {
        let (engine, storage) = test_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        // A repository, so the only thing that can stop the write is the
        // missing project.
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        let before = graph_rows(&storage);

        for name in ["scratch.rs", "notes.yml"] {
            let file = tmp.path().join(name);
            std::fs::write(&file, "fn scratch_fn() { helper(); }\nfn helper() {}\n").unwrap();
            assert!(
                crate::codemap::identity::is_temp_path(file.parent().unwrap()),
                "fixture must be a temp path"
            );
            update_code_graph_with(&write_input(&file, None), &engine, resolve_without_env)
                .unwrap();
            update_code_graph_with(
                &write_input(&file, Some("  ")),
                &engine,
                resolve_without_env,
            )
            .unwrap();
        }
        assert_eq!(graph_rows(&storage), before, "nothing may be written");
        assert!(before.iter().all(|(_, n)| *n == 0));
    }

    #[test]
    fn a_scratch_file_with_a_session_cwd_is_filed_under_that_project() {
        let (engine, storage) = test_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("scratch.rs");
        std::fs::write(&file, "fn scratch_fn() { helper(); }\nfn helper() {}\n").unwrap();
        update_code_graph_with(
            &write_input(&file, Some("/Users/x/projects/my-repo")),
            &engine,
            resolve_without_env,
        )
        .unwrap();
        let projects: Vec<String> = storage
            .with_connection(|c| {
                let mut stmt = c.prepare("SELECT DISTINCT project FROM code_nodes")?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
            })
            .unwrap();
        assert_eq!(projects, vec!["my-repo".to_string()]);
    }

    #[tokio::test]
    async fn code_evolution_skips_a_scratch_file_with_no_project() {
        let (engine, storage) = test_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        // One supported-language file (structural diff branch) and one
        // unsupported (touch-row branch); both would insert if a project existed.
        for name in ["scratch.rs", "notes.yml"] {
            let file = tmp.path().join(name);
            std::fs::write(&file, "fn scratch_fn() {}\n").unwrap();
            track_code_evolution_with(&write_input(&file, None), &engine, resolve_without_env)
                .await
                .unwrap();
        }
        assert_eq!(
            graph_rows(&storage)
                .into_iter()
                .find(|(t, _)| t == "code_evolution")
                .unwrap()
                .1,
            0
        );

        // Control: the same two edits with a session cwd do land.
        for name in ["scratch.rs", "notes.yml"] {
            let file = tmp.path().join(name);
            track_code_evolution_with(
                &write_input(&file, Some("/Users/x/projects/my-repo")),
                &engine,
                resolve_without_env,
            )
            .await
            .unwrap();
        }
        let projects: Vec<String> = storage
            .with_connection(|c| {
                let mut stmt = c.prepare("SELECT DISTINCT project_name FROM code_evolution")?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
            })
            .unwrap();
        assert_eq!(projects, vec!["my-repo".to_string()]);
    }

    fn queue_rows(storage: &crate::storage::Storage) -> Vec<String> {
        storage
            .with_connection(crate::codemap::store::queue_read)
            .unwrap()
            .into_iter()
            .map(|(p, _)| p)
            .collect()
    }

    fn edit_input(tool: &str, file_path: &str) -> crate::hooks::HookInput {
        crate::hooks::HookInput {
            tool_name: Some(tool.into()),
            tool_input: Some(serde_json::json!({ "file_path": file_path })),
            ..Default::default()
        }
    }

    #[test]
    fn enqueue_for_codemap_queues_an_absolute_code_path_for_edit_write_and_multiedit() {
        let (engine, storage) = test_engine();
        for (i, tool) in ["Edit", "Write", "MultiEdit"].iter().enumerate() {
            let p = format!("/w/repo/src/f{i}.rs");
            enqueue_for_codemap(&edit_input(tool, &p), &engine).unwrap();
        }
        let mut rows = queue_rows(&storage);
        rows.sort();
        assert_eq!(
            rows,
            vec![
                "/w/repo/src/f0.rs",
                "/w/repo/src/f1.rs",
                "/w/repo/src/f2.rs"
            ]
        );
        // The same file twice stays one row.
        enqueue_for_codemap(&edit_input("Edit", "/w/repo/src/f0.rs"), &engine).unwrap();
        assert_eq!(queue_rows(&storage).len(), 3);
    }

    #[test]
    fn enqueue_for_codemap_skips_markdown_relative_and_protected_paths() {
        let (engine, storage) = test_engine();
        for p in [
            "/w/repo/README.md",
            "/w/repo/notes.txt",
            "relative/a.rs",
            "/Users/nobody/Desktop/proj/a.rs",
            "/Users/nobody/Documents/proj/a.py",
            "/Users/nobody/Downloads/a.ts",
        ] {
            enqueue_for_codemap(&edit_input("Write", p), &engine).unwrap();
        }
        assert!(queue_rows(&storage).is_empty());
    }

    #[tokio::test]
    async fn handle_enqueues_for_write_but_not_for_read_and_survives_a_missing_table() {
        let (engine, storage) = test_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path();
        handle(&edit_input("Read", "/w/repo/src/a.rs"), &engine, cwd)
            .await
            .unwrap();
        assert!(queue_rows(&storage).is_empty(), "Read is not an edit");
        handle(&edit_input("Write", "/w/repo/src/a.rs"), &engine, cwd)
            .await
            .unwrap();
        assert_eq!(queue_rows(&storage), vec!["/w/repo/src/a.rs"]);

        storage
            .with_connection(|c| {
                c.execute_batch("DROP TABLE codemap_queue")?;
                Ok(())
            })
            .unwrap();
        handle(&edit_input("Write", "/w/repo/src/b.rs"), &engine, cwd)
            .await
            .expect("the hook still returns Ok when the queue table is missing");
    }

    #[test]
    fn enqueue_for_codemap_gives_up_fast_when_the_writer_lock_is_held() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("csr-engine.db");
        let storage = std::sync::Arc::new(crate::storage::Storage::open(&db).unwrap());
        let embeddings = std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap());
        let search = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::search::SearchEngine::new(100),
        ));
        let engine = crate::engine::Engine::from_parts(
            storage.clone(),
            embeddings,
            search,
            dir.path().to_path_buf(),
        );
        // Another process (the daemon) holds the write lock.
        let holder = rusqlite::Connection::open(&db).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();

        let started = std::time::Instant::now();
        let outcome = enqueue_for_codemap(&edit_input("Edit", "/w/repo/src/locked.rs"), &engine);
        let took = started.elapsed();
        assert!(
            outcome.is_err(),
            "contention must surface as a skipped enqueue"
        );
        assert!(
            took < std::time::Duration::from_millis(500),
            "enqueue waited {took:?} on a held write lock"
        );

        holder.execute_batch("ROLLBACK").unwrap();
        assert!(queue_rows(&storage).is_empty(), "the skipped row is absent");
        let timeout_ms: i64 = storage
            .with_connection(|c| Ok(c.query_row("PRAGMA busy_timeout", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(
            timeout_ms,
            crate::storage::DEFAULT_BUSY_TIMEOUT.as_millis() as i64,
            "the default busy timeout is restored after the failed enqueue"
        );
        // And a later enqueue, with the lock free, lands.
        enqueue_for_codemap(&edit_input("Edit", "/w/repo/src/free.rs"), &engine).unwrap();
        assert_eq!(queue_rows(&storage), vec!["/w/repo/src/free.rs"]);
    }

    fn signal_rows(storage: &crate::storage::Storage) -> Vec<(String, String)> {
        let mut rows: Vec<(String, String)> = storage
            .with_connection(|c| crate::codemap::store::list_signals(c, 0))
            .unwrap()
            .into_iter()
            .map(|s| (s.cwd, s.source))
            .collect();
        rows.sort();
        rows
    }

    fn input_in(cwd: Option<&str>, tool: &str, file_path: &str) -> crate::hooks::HookInput {
        crate::hooks::HookInput {
            cwd: cwd.map(str::to_string),
            ..edit_input(tool, file_path)
        }
    }

    #[tokio::test]
    async fn every_post_tool_use_fire_records_its_cwd_as_a_signal() {
        let (engine, storage) = test_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        // A Read is not an edit, but the session is live: it still signals.
        handle(
            &input_in(Some("/w/repo/src"), "Read", "/w/repo/src/a.rs"),
            &engine,
            tmp.path(),
        )
        .await
        .unwrap();
        handle(
            &input_in(Some("/w/repo/src"), "Edit", "/w/repo/src/a.rs"),
            &engine,
            tmp.path(),
        )
        .await
        .unwrap();
        handle(
            &input_in(Some("/w/other"), "Write", "/w/other/b.rs"),
            &engine,
            tmp.path(),
        )
        .await
        .unwrap();
        assert_eq!(
            signal_rows(&storage),
            vec![
                ("/w/other".to_string(), "post_tool_use".to_string()),
                ("/w/repo/src".to_string(), "post_tool_use".to_string()),
            ],
            "one row per cwd"
        );
    }

    #[tokio::test]
    async fn a_missing_cwd_or_missing_signals_table_does_not_fail_the_hook() {
        let (engine, storage) = test_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        for cwd in [None, Some(""), Some("relative/dir")] {
            handle(
                &input_in(cwd, "Read", "/w/repo/src/a.rs"),
                &engine,
                tmp.path(),
            )
            .await
            .unwrap();
        }
        assert!(signal_rows(&storage).is_empty(), "nothing to record");
        storage
            .with_connection(|c| {
                c.execute_batch("DROP TABLE codemap_signals")?;
                Ok(())
            })
            .unwrap();
        handle(
            &input_in(Some("/w/repo"), "Write", "/w/repo/src/a.rs"),
            &engine,
            tmp.path(),
        )
        .await
        .expect("the hook still returns Ok when the signals table is missing");
        // The edit's queue row was not lost to the failed signal.
        assert_eq!(queue_rows(&storage), vec!["/w/repo/src/a.rs"]);
    }

    #[test]
    fn the_signal_gives_up_fast_when_the_writer_lock_is_held_and_restores_the_timeout() {
        let dir = tempfile::TempDir::new().unwrap();
        let db = dir.path().join("csr-engine.db");
        let storage = std::sync::Arc::new(crate::storage::Storage::open(&db).unwrap());
        let embeddings = std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap());
        let search = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::search::SearchEngine::new(100),
        ));
        let engine = crate::engine::Engine::from_parts(
            storage.clone(),
            embeddings,
            search,
            dir.path().to_path_buf(),
        );
        let holder = rusqlite::Connection::open(&db).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();

        let started = std::time::Instant::now();
        crate::hooks::record_codemap_signal(
            &input_in(Some("/w/repo"), "Edit", "/w/repo/a.rs"),
            &engine,
            "post_tool_use",
        );
        let took = started.elapsed();
        assert!(
            took < std::time::Duration::from_millis(500),
            "the signal waited {took:?} on a held write lock"
        );
        holder.execute_batch("ROLLBACK").unwrap();
        assert!(
            signal_rows(&storage).is_empty(),
            "the skipped row is absent"
        );
        let timeout_ms: i64 = storage
            .with_connection(|c| Ok(c.query_row("PRAGMA busy_timeout", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(
            timeout_ms,
            crate::storage::DEFAULT_BUSY_TIMEOUT.as_millis() as i64,
            "the default busy timeout is restored after the skipped signal"
        );
        crate::hooks::record_codemap_signal(
            &input_in(Some("/w/repo"), "Edit", "/w/repo/a.rs"),
            &engine,
            "post_tool_use",
        );
        assert_eq!(
            signal_rows(&storage),
            vec![("/w/repo".to_string(), "post_tool_use".to_string())]
        );
    }

    fn test_engine() -> (
        crate::engine::Engine,
        std::sync::Arc<crate::storage::Storage>,
    ) {
        let storage = std::sync::Arc::new(crate::storage::Storage::open_memory().unwrap());
        let embeddings = std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap());
        let search = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::search::SearchEngine::new(100),
        ));
        let engine = crate::engine::Engine::from_parts(
            storage.clone(),
            embeddings,
            search,
            std::path::PathBuf::from("/tmp"),
        );
        (engine, storage)
    }

    #[tokio::test]
    async fn track_code_evolution_records_touch_row_for_unsupported_extension() {
        let (engine, _storage) = test_engine();
        // A presence row is now bounded to files inside a repository, so the
        // fixture must be one — a bare /tmp path is correctly ignored.
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        let repo_file = tmp.path().join("csr-test-workflow-file.yml");
        std::fs::write(&repo_file, "name: old\n").unwrap();
        let repo_file_str = repo_file.to_string_lossy().to_string();
        let input = crate::hooks::HookInput {
            transcript_path: Some("/tmp/nonexistent-test-transcript.jsonl".to_string()),
            session_id: Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into()),
            cwd: Some("/tmp".into()),
            tool_name: Some("Edit".into()),
            tool_input: Some(serde_json::json!({
                "file_path": repo_file_str,
                "old_string": "name: old",
                "new_string": "name: new"
            })),
            ..Default::default()
        };

        let result = track_code_evolution(&input, &engine).await;
        assert!(result.is_ok());

        // LIKE lookup: `canonical_repo_path` may spell `/tmp` vs `/private/tmp`
        // depending on cache state; match the basename instead.
        let (fa, fr): (String, String) = engine
            .storage()
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT functions_added, functions_removed FROM code_evolution \
                     WHERE file_path LIKE ?1",
                    [format!("%{}", "csr-test-workflow-file.yml")],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(Into::into)
            })
            .expect("unsupported extension should insert a code_evolution touch row");
        assert_eq!(fa, "[]");
        assert_eq!(fr, "[]");
    }

    /// The touch-row branch fires for EVERY unsupported extension, so it must be
    /// bounded. A file outside any repository, and a credential or vendored path
    /// inside one, must never be recorded — the stored path is itself the leak.
    #[tokio::test]
    async fn track_code_evolution_touch_row_is_bounded_to_repo_and_skips_secrets() {
        let (engine, _storage) = test_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();

        let secret = tmp.path().join(".env");
        std::fs::write(&secret, "TOKEN=shhh\n").unwrap();
        let vendored = tmp.path().join("node_modules").join("pkg").join("a.yml");
        std::fs::create_dir_all(vendored.parent().unwrap()).unwrap();
        std::fs::write(&vendored, "name: dep\n").unwrap();
        // Outside any repository: no .git anywhere above it.
        let outside_dir = tempfile::TempDir::new().unwrap();
        let outside = outside_dir.path().join("loose.yml");
        std::fs::write(&outside, "name: loose\n").unwrap();

        for path in [&secret, &vendored, &outside] {
            let input = crate::hooks::HookInput {
                transcript_path: Some("/tmp/nonexistent-test-transcript.jsonl".to_string()),
                session_id: Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into()),
                cwd: Some(tmp.path().to_string_lossy().to_string()),
                tool_name: Some("Edit".into()),
                tool_input: Some(serde_json::json!({
                    "file_path": path.to_string_lossy().to_string(),
                    "old_string": "a",
                    "new_string": "b"
                })),
                ..Default::default()
            };
            track_code_evolution(&input, &engine).await.unwrap();
        }

        let rows: i64 = engine
            .storage()
            .with_connection(|conn| {
                conn.query_row("SELECT COUNT(*) FROM code_evolution", [], |r| r.get(0))
                    .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(
            rows, 0,
            "secrets, vendored and non-repo paths must not be recorded"
        );
    }

    #[tokio::test]
    async fn track_code_evolution_touch_row_language_is_placeholder_not_empty() {
        let (engine, _storage) = test_engine();
        // A presence row is now bounded to files inside a repository, so the
        // fixture must be one — a bare /tmp path is correctly ignored.
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        let repo_file = tmp.path().join("csr-test-workflow-file.yml");
        std::fs::write(&repo_file, "name: old\n").unwrap();
        let repo_file_str = repo_file.to_string_lossy().to_string();
        let input = crate::hooks::HookInput {
            transcript_path: Some("/tmp/nonexistent-test-transcript.jsonl".to_string()),
            session_id: Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into()),
            cwd: Some("/tmp".into()),
            tool_name: Some("Edit".into()),
            tool_input: Some(serde_json::json!({
                "file_path": repo_file_str,
                "old_string": "name: old",
                "new_string": "name: new"
            })),
            ..Default::default()
        };

        let result = track_code_evolution(&input, &engine).await;
        assert!(result.is_ok());

        let language: String = engine
            .storage()
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT language FROM code_evolution WHERE file_path LIKE ?1",
                    [format!("%{}", "csr-test-workflow-file.yml")],
                    |row| row.get::<_, String>(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(language, "text");
        assert!(!language.is_empty());
    }

    #[tokio::test]
    async fn track_code_evolution_still_populates_symbols_for_supported_language() {
        let (engine, _storage) = test_engine();
        let input = crate::hooks::HookInput {
            transcript_path: Some("/tmp/nonexistent-test-transcript.jsonl".to_string()),
            session_id: Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into()),
            cwd: Some("/tmp".into()),
            tool_name: Some("Write".into()),
            tool_input: Some(serde_json::json!({
                "file_path": "/tmp/csr-test-evolved-file.rs",
                "content": "fn touched_marker_fn() {}\n"
            })),
            ..Default::default()
        };

        let result = track_code_evolution(&input, &engine).await;
        assert!(result.is_ok());

        let functions_added: String = engine
            .storage()
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT functions_added FROM code_evolution WHERE file_path LIKE ?1",
                    [format!("%{}", "csr-test-evolved-file.rs")],
                    |row| row.get(0),
                )
                .map_err(Into::into)
            })
            .expect("supported language with structural change should insert a row");
        assert_ne!(
            functions_added, "[]",
            "functions_added must contain real AST diff, not an empty touch array"
        );
    }

    #[test]
    fn code_graph_records_latest_chunk_only_for_changed_nodes() {
        let (engine, storage) = test_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("lib.rs");
        std::fs::write(&file, "fn changed() { 1; }\nfn sibling() { 2; }\n").unwrap();
        let canonical_file = file.canonicalize().unwrap();
        let input = crate::hooks::HookInput {
            transcript_path: Some(tmp.path().join("conv.jsonl").to_string_lossy().to_string()),
            session_id: Some("session".into()),
            // The file is under a temp root, so its project comes from the
            // session cwd; with none the graph write is skipped.
            cwd: Some("/Users/x/projects/proj".into()),
            tool_name: Some("Edit".into()),
            tool_input: Some(serde_json::json!({
                "file_path": canonical_file.to_string_lossy().to_string()
            })),
            ..Default::default()
        };
        let chunk = |id: &str, seq: usize| crate::import::ConversationChunk {
            id: id.into(),
            conversation_id: "conv".into(),
            project_name: "proj".into(),
            timestamp: "2026-08-09T00:00:00Z".into(),
            content: "tool edit".into(),
            message_count: 1,
            summary: None,
            author: crate::provenance::Speaker::Assistant,
            seq,
            is_sidechain: false,
        };

        storage
            .insert_chunk(&chunk("chunk-1", 0), &[0.0; 4])
            .unwrap();
        update_code_graph(&input, &engine).unwrap();

        storage
            .insert_chunk(&chunk("chunk-2", 1), &[0.0; 4])
            .unwrap();
        std::fs::write(
            &canonical_file,
            "fn changed() { 3; }\nfn sibling() { 2; }\n",
        )
        .unwrap();
        update_code_graph(&input, &engine).unwrap();

        let attributions: std::collections::HashMap<String, Option<String>> = storage
            .with_connection(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT name, last_chunk_id FROM code_nodes
                     WHERE name IN ('changed', 'sibling')",
                )?;
                let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
                rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
            })
            .unwrap();
        assert_eq!(attributions.get("changed"), Some(&Some("chunk-2".into())));
        assert_eq!(
            attributions.get("sibling"),
            Some(&Some("chunk-1".into())),
            "an unchanged sibling must not inherit the later edit chunk"
        );
    }

    fn bash_input(command: &str, cwd: Option<&str>) -> crate::hooks::HookInput {
        crate::hooks::HookInput {
            transcript_path: Some("/tmp/nonexistent-test-transcript.jsonl".to_string()),
            session_id: Some("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into()),
            cwd: cwd.map(str::to_string),
            tool_name: Some("Bash".into()),
            tool_input: Some(serde_json::json!({"command": command})),
            ..Default::default()
        }
    }

    fn evolution_rows(storage: &crate::storage::Storage) -> Vec<(String, String, String, String)> {
        storage
            .with_connection(|c| {
                let mut stmt = c.prepare(
                    "SELECT tool_name, file_path, functions_added, language FROM code_evolution",
                )?;
                let rows =
                    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
                rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
            })
            .unwrap()
    }

    /// A temp directory that counts as a repository for the presence-row bound.
    fn repo_dir() -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        tmp
    }

    #[test]
    fn bash_written_files_lists_only_existing_files_a_bash_call_wrote() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().to_string_lossy().to_string();
        std::fs::write(tmp.path().join("a.rs"), "fn a() {}\n").unwrap();
        let input = bash_input(
            "sed -i '' 's/a/b/' a.rs && echo x > not-made.txt",
            Some(&cwd),
        );
        assert_eq!(bash_written_files(&input), [tmp.path().join("a.rs")]);

        // Another tool never goes through the command reader.
        let mut other = input.clone();
        other.tool_name = Some("Read".into());
        assert!(bash_written_files(&other).is_empty());
        // No command, no files.
        let mut bare = input;
        bare.tool_input = None;
        assert!(bash_written_files(&bare).is_empty());
    }

    #[test]
    fn bash_written_files_is_capped() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().to_string_lossy().to_string();
        let mut command = String::new();
        for n in 0..(MAX_BASH_FILES + 4) {
            std::fs::write(tmp.path().join(format!("f{n}.txt")), "x").unwrap();
            command.push_str(&format!("echo y > f{n}.txt; "));
        }
        let files = bash_written_files(&bash_input(&command, Some(&cwd)));
        assert_eq!(files.len(), MAX_BASH_FILES);
    }

    #[test]
    fn only_a_bash_call_that_wrote_nothing_exits_before_the_engine() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().to_string_lossy().to_string();
        std::fs::write(tmp.path().join("a.rs"), "fn a() {}\n").unwrap();
        let reads = bash_input("git status && ls -la", Some(&cwd));
        let writes = bash_input("echo x > a.rs", Some(&cwd));
        assert!(exits_before_engine("post-tool-use", &reads));
        assert!(!exits_before_engine("post-tool-use", &writes));
        // Not Bash, not this hook: always on to the engine.
        assert!(!exits_before_engine(
            "post-tool-use",
            &edit_input("Edit", "/w/a.rs")
        ));
        assert!(!exits_before_engine("stop", &reads));
    }

    #[tokio::test]
    async fn a_bash_call_that_wrote_nothing_leaves_every_table_alone() {
        let (engine, storage) = test_engine();
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd = tmp.path().to_string_lossy().to_string();
        let before = graph_rows(&storage);
        handle(
            &bash_input("git status && cargo test", Some(&cwd)),
            &engine,
            tmp.path(),
        )
        .await
        .unwrap();
        assert_eq!(graph_rows(&storage), before);
        assert!(queue_rows(&storage).is_empty());
        assert!(
            signal_rows(&storage).is_empty(),
            "not even the code map signal"
        );
    }

    #[tokio::test]
    async fn a_bash_write_takes_the_path_an_edit_takes_with_tool_name_bash() {
        let (engine, storage) = test_engine();
        let tmp = repo_dir();
        let rs = tmp.path().join("lib.rs");
        let yml = tmp.path().join("ci.yml");
        std::fs::write(&rs, "fn fresh_fn() { helper(); }\nfn helper() {}\n").unwrap();
        std::fs::write(&yml, "name: x\n").unwrap();
        let command = format!(
            "sed -i '' 's/a/b/' {} && echo y >> {}",
            rs.display(),
            yml.display()
        );
        let input = bash_input(&command, Some("/Users/x/projects/my-repo"));

        handle(&input, &engine, tmp.path()).await.unwrap();

        let rows = evolution_rows(&storage);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows.iter().all(|r| r.0 == "Bash"));
        let by_file = |name: &str| rows.iter().find(|r| r.1.ends_with(name)).unwrap();
        // A supported language gets its language and EMPTY symbol lists: no
        // diff is invented for a command that carries no before/after text.
        assert_eq!(by_file("lib.rs").2, "[]");
        assert_eq!(by_file("lib.rs").3, "rust");
        assert_eq!(by_file("ci.yml").3, "text");
        // The code graph was re-extracted from the file as it is on disk.
        let graph = graph_rows(&storage);
        let n = |t: &str| graph.iter().find(|(name, _)| name == t).unwrap().1;
        assert!(n("code_nodes") >= 2, "{graph:?}");
        assert!(n("code_graph_file_state") >= 1, "{graph:?}");
        // The signal ran for this call, as it does for an edit.
        assert_eq!(signal_rows(&storage).len(), 1);
    }

    #[tokio::test]
    async fn a_bash_scratch_file_takes_its_session_project_or_writes_nothing() {
        let (engine, storage) = test_engine();
        let tmp = repo_dir();
        let file = tmp.path().join("scratch.rs");
        let path = file.to_string_lossy().to_string();
        std::fs::write(&file, "fn scratch_fn() { helper(); }\nfn helper() {}\n").unwrap();
        let command = format!("echo y > {path}");

        // No session project: the file history and the graph stay empty.
        let no_project = bash_input(&command, None);
        track_code_evolution_for(&no_project, &engine, &path, resolve_without_env)
            .await
            .unwrap();
        update_code_graph_for(&no_project, &engine, &path, resolve_without_env).unwrap();
        assert!(graph_rows(&storage).iter().all(|(_, n)| *n == 0));

        // With one, the row lands under it.
        let with_project = bash_input(&command, Some("/Users/x/projects/my-repo"));
        track_code_evolution_for(&with_project, &engine, &path, resolve_without_env)
            .await
            .unwrap();
        let projects: Vec<String> = storage
            .with_connection(|c| {
                let mut stmt = c.prepare("SELECT DISTINCT project_name FROM code_evolution")?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
            })
            .unwrap();
        assert_eq!(projects, vec!["my-repo".to_string()]);
    }

    #[tokio::test]
    async fn a_bash_write_to_a_vendored_path_is_not_recorded() {
        let (engine, storage) = test_engine();
        let tmp = repo_dir();
        let vendored = tmp.path().join("node_modules").join("pkg").join("index.js");
        std::fs::create_dir_all(vendored.parent().unwrap()).unwrap();
        std::fs::write(&vendored, "function a() {}\n").unwrap();
        let input = bash_input("echo y", Some("/Users/x/projects/my-repo"));
        track_code_evolution_for(
            &input,
            &engine,
            &vendored.to_string_lossy(),
            resolve_without_env,
        )
        .await
        .unwrap();
        assert!(evolution_rows(&storage).is_empty());
    }
}
