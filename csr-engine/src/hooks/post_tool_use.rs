//! PostToolUse hook — tracks code evolution.
//!
//! Fires after each successful tool call. For Edit/Write operations, tracks
//! structural code changes (functions/types/imports added/removed). It does not
//! import the transcript: that embeds through the daemon and can wait minutes
//! while Claude Code waits on this hook. The daemon's watcher and the detached
//! Stop worker at the end of every turn index the transcript.
//!
//! A Bash call is read first (`hooks::bash_writes`): one that wrote no file
//! returns before the engine is built, so the most frequent tool costs a
//! process start and a parse. One that wrote files takes the same path an Edit
//! takes, once per file, with `tool_name = 'Bash'` and no symbol diff (a shell
//! command carries no before/after text, and a diff is never invented).
//!
//! Always returns Ok(()) — never blocks Claude Code (C-2 fix).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;

use super::HookInput;
use crate::engine::Engine;

/// Tool names whose edits this hook tracks (code evolution + code graph).
/// The installer's PostToolUse matcher (`Edit|Write|MultiEdit|NotebookEdit|Bash`,
/// see `hooks::install`) is deliberately a superset of this list. `Bash` is
/// not here: it names no file in `tool_input`, so it has its own path through
/// [`bash_written_files`].
pub fn is_acted_on_tool(tool_name: Option<&str>) -> bool {
    matches!(tool_name, Some("Edit") | Some("Write") | Some("MultiEdit"))
}

/// Wall-clock budget for the database writes of one PostToolUse run, counted
/// from the start of [`handle`]. The hook does not extract the edited file or
/// resolve anything: its graph write is one snapshot INSERT (or a spool file,
/// `extraction::graph_spool`) and the drain does the rest. What can stall the
/// hook is the write lock, which the storage default would wait on for 5 s per
/// statement. Each write phase waits only as long as the budget has left and
/// the snapshot never waits more than [`SNAPSHOT_WAIT`]; once the budget is
/// spent the remaining file-history writes are skipped and the hook exits 0.
///
/// Same idea as `prompt_transport::REQUEST_BUDGET`. A backgrounded hook
/// (`async: true`) is not the answer: a stalled one stays a live csr-engine
/// process.
pub const WRITE_BUDGET: Duration = Duration::from_millis(1500);

/// Cap the wait for the write lock at what is left of the budget; `false` once
/// nothing is left.
fn clamp_write_wait(engine: &Engine, deadline: Instant) -> bool {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return false;
    }
    let _ = engine
        .storage()
        .with_connection(|conn| conn.busy_timeout(left).map_err(Into::into));
    true
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
pub async fn handle(input: &HookInput, engine: &Engine, _cwd: &Path) -> Result<()> {
    handle_with(input, engine, WRITE_BUDGET, resolve_project_for_hook).await
}

/// [`handle`] with the write budget and the project resolver passed in.
async fn handle_with(
    input: &HookInput,
    engine: &Engine,
    budget: Duration,
    resolve: impl Fn(Option<&Path>, Option<&str>) -> String + Copy,
) -> Result<()> {
    let deadline = Instant::now() + budget;
    let bash_files = bash_written_files(input);
    if input.tool_name.as_deref() == Some("Bash") && bash_files.is_empty() {
        return Ok(());
    }
    // Code map session signal: this cwd has a live session. One upsert, first,
    // so the slower steps below cannot delay or starve it.
    super::record_codemap_signal(input, engine, "post_tool_use");
    // A session in a protected folder (Desktop, Documents, ...) is mapped by the
    // session itself: queue the edit and start the detached drain. No-op elsewhere.
    super::codemap_session_post_tool_use(input, engine);

    let touched = touched_files(input, &bash_files);
    // The graph: the hook's only write to it is a snapshot of each edit
    // (`codegraph::insert_snapshot`, or the lock-free spool when the database
    // will not take it quickly). The drain extracts it
    // (`extraction::graph_drain`), so the hook neither parses nor holds the
    // write lock for more than that one INSERT. Every touched file is
    // snapshotted before anything optional runs: the file-history writes below
    // wait on the same lock, and a budget they spend must not cost a Bash call's
    // later files their snapshot.
    let mut db_busy = false;
    for file_path in &touched {
        snapshot_graph_edit(input, engine, file_path, &resolve, deadline, &mut db_busy);
    }
    for file_path in &touched {
        // Code map freshness: enqueue only, the daemon does the work. One
        // INSERT, no parse, no git; a failure (missing table, locked db) is
        // logged and never reaches Claude Code.
        if let Err(e) = enqueue_path_for_codemap(engine, file_path) {
            eprintln!("CSR: code map enqueue error (non-fatal): {}", e);
        }
        // The writes above put the storage default back; the one below waits
        // only as long as the budget has left.
        if !clamp_write_wait(engine, deadline) {
            eprintln!("CSR: write budget spent, file history skipped (non-fatal)");
            break;
        }
        if let Err(e) = track_code_evolution_for(input, engine, file_path, resolve).await {
            eprintln!("CSR: code evolution tracking error (non-fatal): {}", e);
        }
    }

    Ok(())
}

/// The files this call edited or wrote: an Edit/Write/MultiEdit's `file_path`,
/// else the files a Bash call wrote (`bash_files`).
fn touched_files(input: &HookInput, bash_files: &[PathBuf]) -> Vec<String> {
    if is_acted_on_tool(input.tool_name.as_deref()) {
        tool_file_path(input)
            .map(str::to_string)
            .into_iter()
            .collect()
    } else {
        bash_files
            .iter()
            .map(|f| f.to_string_lossy().into_owned())
            .collect()
    }
}

/// How long [`spool_if_database_busy`] waits for the write lock before the
/// edit goes to the spool unopened.
pub const PROBE_WAIT: Duration = Duration::from_millis(500);

/// Before the engine is built: if the database's write lock is held for longer
/// than `wait`, snapshot the call's files into the spool and say so (`true`:
/// the hook is done). Opening the engine runs the migrations, which on an old
/// database need that lock and give up only after the storage default (5 s),
/// far past the hook's budget; a writer that holds it that long must not cost
/// the edit its snapshot. A free lock, a missing database (the normal path
/// creates it) or any other open error is `false` and changes nothing.
pub fn spool_if_database_busy(db_path: &Path, input: &HookInput, wait: Duration) -> bool {
    use rusqlite::{Connection, OpenFlags};
    if !db_path.exists() {
        return false;
    }
    let Ok(conn) = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return false;
    };
    let _ = conn.busy_timeout(wait);
    match conn.execute_batch("BEGIN IMMEDIATE") {
        Ok(()) => {
            let _ = conn.execute_batch("ROLLBACK");
            false
        }
        Err(rusqlite::Error::SqliteFailure(e, _))
            if matches!(
                e.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            let spooled = spool_without_database(db_path, input);
            eprintln!(
                "CSR: database busy, {spooled} graph snapshot(s) spooled unopened (non-fatal)"
            );
            true
        }
        Err(_) => false,
    }
}

/// Snapshot every file this call touched into the spool next to `db_path`,
/// without opening the database. For a hook whose engine could not be built.
/// The transcript chunk the edit belongs to is not looked up (that needs the
/// database), so the extraction credits the edit's conversation and session but
/// no chunk. Returns how many snapshots were written.
pub fn spool_without_database(db_path: &Path, input: &HookInput) -> usize {
    let Some(dir) = crate::extraction::graph_spool::dir_for_db(db_path) else {
        return 0;
    };
    let bash_files = bash_written_files(input);
    let mut written = 0;
    for file_path in touched_files(input, &bash_files) {
        let Some(snapshot) = capture_snapshot(input, &file_path, &resolve_project_for_hook) else {
            continue;
        };
        match crate::extraction::graph_spool::write(&dir, &snapshot) {
            Ok(_) => written += 1,
            Err(e) => eprintln!("CSR: graph snapshot lost: spool: {e:#}"),
        }
    }
    written
}

/// How long the snapshot INSERT may wait for the write lock. Short on purpose:
/// the alternative to waiting is the spool, which costs one small file and no
/// lock, so the hook never queues behind a long writer for the graph.
const SNAPSHOT_WAIT: Duration = Duration::from_millis(150);

/// What the hook saw of `file_path`, ready to queue: `None` when there is
/// nothing to hand the drain (no project, or a supported file that cannot be
/// read as text). Carries no transcript chunk; the caller adds it when it has
/// a database.
fn capture_snapshot(
    input: &HookInput,
    file_path: &str,
    resolve: &impl Fn(Option<&Path>, Option<&str>) -> String,
) -> Option<crate::storage::codegraph::Snapshot> {
    use crate::storage::codegraph::{self, Snapshot};
    // No project, no graph write: `resolve_code_edges("")` and
    // `compute_code_rank("")` read `''` as EVERY project (`?1 = '' OR project =
    // ?1`), so a scratch file with no session project would bind a call in one
    // project to a definition in another. A node filed under `''` is no better:
    // no scoped query would ever find it.
    let project = resolve(Path::new(file_path).parent(), input.cwd.as_deref());
    if project.is_empty() {
        return None;
    }
    let conv_id = conv_id_for(input);
    let session_id = input.session_id.clone().unwrap_or_default();
    let supported = crate::extraction::ast_analysis::lang_from_path_str(file_path).is_some();
    // The capture time is taken after the read: the text is at least as old as
    // its `seq`, and a later edit of the file always has a larger one.
    let content = if supported {
        // Unreadable, not UTF-8 or too large: nothing to extract.
        Some(crate::extraction::file_extract::read_for_snapshot(
            file_path,
        )?)
    } else {
        None
    };
    let seq = codegraph::now_ns();
    // Canonicalized only for a file that is going to be stored, as the
    // extraction did before the snapshot: `canonical_repo_path` spells a
    // directory without a `.git` as given on its first lookup in a process and
    // resolved after that, so a file that is skipped must not use the first one.
    let file = crate::extraction::repo_path::canonical_repo_path(Path::new(file_path))
        .to_string_lossy()
        .into_owned();
    Some(Snapshot {
        seq,
        project,
        file,
        read_path: file_path.to_string(),
        kind: if supported {
            codegraph::SNAPSHOT_EXTRACT
        } else {
            codegraph::SNAPSHOT_UNSUPPORTED
        }
        .to_string(),
        content,
        conv_id,
        session_id,
        source_chunk_id: None,
        origin: None,
    })
}

/// Hand the edit of `file_path` to the graph drain: one snapshot row, or, when
/// the database does not take it within [`SNAPSHOT_WAIT`] (or the remaining
/// budget), one spool file. Never extracts, never parses, never fails the hook.
/// `db_busy` is set by the first file that could not be inserted, and the
/// files after it go straight to the spool instead of each waiting out the
/// same lock.
fn snapshot_graph_edit(
    input: &HookInput,
    engine: &Engine,
    file_path: &str,
    resolve: &impl Fn(Option<&Path>, Option<&str>) -> String,
    deadline: Instant,
    db_busy: &mut bool,
) {
    use crate::storage::codegraph;
    let Some(mut snapshot) = capture_snapshot(input, file_path, resolve) else {
        return;
    };
    let db_error = if *db_busy {
        anyhow::anyhow!("an earlier snapshot of this call found the database locked")
    } else {
        if snapshot.content.is_some() {
            snapshot.source_chunk_id = engine
                .storage()
                .with_connection(|conn| codegraph::latest_chunk_id(conn, &snapshot.conv_id))
                .ok()
                .flatten();
        }
        let wait = SNAPSHOT_WAIT.min(deadline.saturating_duration_since(Instant::now()));
        let inserted = engine.storage().with_connection(|conn| {
            conn.busy_timeout(wait)?;
            codegraph::insert_snapshot(conn, &snapshot)
        });
        match inserted {
            Ok(_) => return,
            Err(e) => {
                *db_busy = true;
                e
            }
        }
    };
    let dir = engine
        .storage()
        .with_connection(|conn| Ok(crate::extraction::graph_spool::dir_for_conn(conn)))
        .ok()
        .flatten();
    match dir.map(|d| crate::extraction::graph_spool::write(&d, &snapshot)) {
        Some(Ok(_)) => eprintln!("CSR: graph snapshot spooled ({db_error:#}) (non-fatal)"),
        Some(Err(e)) => eprintln!("CSR: graph snapshot lost: {db_error:#}; spool: {e:#}"),
        None => eprintln!("CSR: graph snapshot lost: {db_error:#}; no spool directory"),
    }
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

/// The hook's graph step followed by the extraction the drain would make of it
/// a moment later, so tests can look at the graph an edit leads to.
#[cfg(test)]
fn update_code_graph(input: &HookInput, engine: &Engine) -> Result<()> {
    update_code_graph_with(input, engine, resolve_project_for_hook)
}

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

#[cfg(test)]
fn update_code_graph_for(
    input: &HookInput,
    engine: &Engine,
    file_path: &str,
    resolve: impl Fn(Option<&Path>, Option<&str>) -> String,
) -> Result<()> {
    snapshot_graph_edit(
        input,
        engine,
        file_path,
        &resolve,
        Instant::now() + WRITE_BUDGET,
        &mut false,
    );
    drain_snapshots_for_test(engine)
}

/// Run the extraction half of a drain pass (no resolve).
#[cfg(test)]
fn drain_snapshots_for_test(engine: &Engine) -> Result<()> {
    engine.storage().with_connection(|conn| {
        let out = crate::extraction::graph_drain::extract_all_snapshots(conn)?;
        anyhow::ensure!(out.errors.is_empty(), "{:?}", out.errors);
        Ok(())
    })
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
#[path = "post_tool_use_tests.rs"]
mod tests;
