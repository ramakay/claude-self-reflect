//! Code-graph backfill (v9.4).
//!
//! The PostToolUse hook (`hooks/post_tool_use.rs::update_code_graph`) only
//! populates the conversation-provenance code graph for edits made *going
//! forward*. This command replays the entire JSONL conversation history and
//! reconstructs the graph from every `Edit` / `Write` / `MultiEdit` tool_use
//! ever recorded — so callers/callees/file-ledger work without waiting for each
//! file to be touched again.
//!
//! Ordering matters for provenance:
//! - Conversations are processed **oldest → newest** (sorted by file mtime).
//!   `upsert_code_node` preserves `first_conv_id` (immutable CASE in the SQL),
//!   so the OLDEST conversation that introduced a symbol owns `first_conv_id`,
//!   while `last_conv_id` tracks the newest sighting.
//! - The **newest** conversation touching a file owns that file's edges
//!   (current state). We keep a per-`(project, file)` map of the latest
//!   fragment's edges and flush once at the end, so later conversations win.
//!
//! Source of truth (v9.4.1): when the touched file still exists on disk (the
//! common case — ~90% of historically-edited files), we extract the graph from
//! the **current on-disk file**, exactly like the live hook. The complete file
//! contains both callers and callees, so edges resolve instead of dangling as
//! `name:` placeholders. Conversation history supplies *provenance* (which
//! conversation first/last touched the file); disk supplies *structure*.
//!
//! Only when the file is gone (deleted / moved / never on this machine) do we
//! fall back to concatenating the edit *fragments* (`new_string` / `content`)
//! and extracting from that blob — best-effort symbol recovery from history.
//!
//! Robustness: per-conversation and per-write errors are logged and skipped,
//! never fatal (mirrors the rest of the codebase). Tree-sitter is wrapped by the
//! existing `catch_unwind` inside `extract_graph_fragment`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use anyhow::Result;

use crate::engine::Engine;
use crate::extraction::ast_analysis::lang_from_path_str;
use crate::extraction::codegraph::extract_graph_fragment;
use crate::import::{
    discover_projects, list_jsonl_files, normalize_project_name, parse_jsonl_file,
    parse_jsonl_messages,
};
use crate::storage::codegraph::EdgeRow;
use crate::storage::Storage;

/// Log progress every N conversations.
const PROGRESS_EVERY: usize = 250;

/// Outcome of a backfill run (also used for the `--dry-run` preview).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BackfillStats {
    /// JSONL conversation files discovered under the projects dir.
    pub files_scanned: usize,
    /// Conversations successfully parsed.
    pub conversations: usize,
    /// Distinct (project, file) pairs that contributed graph fragments.
    pub code_files: usize,
    /// Total node upsert operations (sum across all fragments).
    pub nodes_upserted: usize,
    /// Edges in the final (newest-wins) per-file edge set.
    pub edges_written: usize,
    /// Distinct projects touched.
    pub projects: usize,
    /// Fraction of placeholder edges resolved to a real def (0.0 in dry-run).
    pub resolution_rate: f64,
    /// Edited files in an unsupported language (skipped).
    pub skipped_unsupported: usize,
    /// Distinct files extracted from the live on-disk copy (vs. edit fragments).
    pub files_from_disk: usize,
}

impl BackfillStats {
    /// Human-readable one-block summary.
    pub fn format_text(&self, dry_run: bool) -> String {
        let mode = if dry_run { " (dry-run, no writes)" } else { "" };
        format!(
            "CSR code-graph backfill{mode}\n\
             ─────────────────────────────────\n\
             files scanned       : {}\n\
             conversations       : {}\n\
             code files          : {}\n\
             nodes upserted      : {}\n\
             edges written       : {}\n\
             projects            : {}\n\
             from disk / fragment : {} / {}\n\
             skipped (unsupported): {}\n\
             resolution rate     : {:.1}%\n",
            self.files_scanned,
            self.conversations,
            self.code_files,
            self.nodes_upserted,
            self.edges_written,
            self.projects,
            self.files_from_disk,
            self.code_files.saturating_sub(self.files_from_disk),
            self.skipped_unsupported,
            self.resolution_rate * 100.0,
        )
    }
}

/// Largest on-disk file we will re-parse during backfill (256KB, matches the
/// live hook's `MAX_GRAPH_FILE_BYTES`).
const MAX_DISK_FILE_BYTES: u64 = 256 * 1024;

/// The current on-disk contents of `file_path`, if it exists, is a regular file,
/// and is within the size cap. Cached so a file touched by N conversations is
/// read once. `None` is cached too (file absent / too big / unreadable).
fn disk_source(file_path: &str, cache: &mut BTreeMap<String, Option<String>>) -> Option<String> {
    if let Some(hit) = cache.get(file_path) {
        return hit.clone();
    }
    let val = (|| {
        let meta = std::fs::metadata(file_path).ok()?;
        if !meta.is_file() || meta.len() > MAX_DISK_FILE_BYTES {
            return None;
        }
        std::fs::read_to_string(file_path).ok()
    })();
    cache.insert(file_path.to_string(), val.clone());
    val
}

/// Collect edited code per file from a single JSONL message's tool_use blocks.
/// Appends to `out[file_path]` so multiple edits to the same file accumulate.
fn collect_edits(msg: &serde_json::Value, out: &mut BTreeMap<String, String>) {
    let content = match msg
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        Some(c) => c,
        None => return,
    };

    for item in content {
        if item.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
            continue;
        }
        let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if name != "Edit" && name != "Write" && name != "MultiEdit" {
            continue;
        }
        let input = match item.get("input") {
            Some(i) => i,
            None => continue,
        };
        let file_path = match input.get("file_path").and_then(|v| v.as_str()) {
            Some(f) => f,
            None => continue,
        };

        let mut code = String::new();
        match name {
            "Edit" => {
                if let Some(s) = input.get("new_string").and_then(|v| v.as_str()) {
                    code.push_str(s);
                }
            }
            "Write" => {
                if let Some(s) = input.get("content").and_then(|v| v.as_str()) {
                    code.push_str(s);
                }
            }
            "MultiEdit" => {
                if let Some(edits) = input.get("edits").and_then(|v| v.as_array()) {
                    for e in edits {
                        if let Some(s) = e.get("new_string").and_then(|v| v.as_str()) {
                            code.push_str(s);
                            code.push('\n');
                        }
                    }
                }
            }
            _ => {}
        }

        if code.trim().is_empty() {
            continue;
        }
        let entry = out.entry(file_path.to_string()).or_default();
        if !entry.is_empty() {
            entry.push('\n');
        }
        entry.push_str(&code);
    }
}

/// Best-effort session id for a conversation: the first `sessionId` field seen,
/// else the conversation id.
fn session_id_for(messages: &[serde_json::Value], conv_id: &str) -> String {
    messages
        .iter()
        .find_map(|m| {
            m.get("sessionId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| conv_id.to_string())
}

/// Backfill the code graph from all existing conversation JSONL files.
///
/// `dry_run` performs all parsing + extraction + counting but writes nothing.
pub fn backfill_code_graph(
    engine: &Engine,
    projects_dir: &Path,
    dry_run: bool,
) -> Result<BackfillStats> {
    backfill_into(engine.storage(), projects_dir, dry_run)
}

/// Storage-level backfill (no embeddings needed — keeps tests light).
fn backfill_into(storage: &Storage, projects_dir: &Path, dry_run: bool) -> Result<BackfillStats> {
    let mut stats = BackfillStats::default();

    // 1. Enumerate every JSONL file across all project dirs, tagged with its
    //    project name and mtime.
    let projects = discover_projects(projects_dir)?;
    let mut files: Vec<(PathBuf, String, SystemTime)> = Vec::new();
    for (dir, project_name) in &projects {
        let jsonls = match list_jsonl_files(dir) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("CSR backfill: cannot list {} ({e})", dir.display());
                continue;
            }
        };
        for path in jsonls {
            let mtime = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            files.push((path, project_name.clone(), mtime));
        }
    }

    // 2. Chronological order: oldest first, so first_conv_id lands on the
    //    earliest conversation that introduced a symbol.
    files.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
    stats.files_scanned = files.len();

    // Newest-wins per-file edge set + content hash (flushed once at the end).
    let mut latest_edges: BTreeMap<(String, String), Vec<EdgeRow>> = BTreeMap::new();
    let mut latest_hash: BTreeMap<(String, String), String> = BTreeMap::new();
    let mut touched_projects: BTreeSet<String> = BTreeSet::new();
    // Cache of on-disk file contents (Some) / absence (None), keyed by path.
    let mut disk_cache: BTreeMap<String, Option<String>> = BTreeMap::new();
    // Files we sourced from disk (counted once, on first sighting).
    let mut from_disk_files: BTreeSet<(String, String)> = BTreeSet::new();

    let total_files = files.len();
    for (i, (path, project_name, _mtime)) in files.iter().enumerate() {
        let conv_id = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let messages = match parse_jsonl_messages(path) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("CSR backfill: skip {} ({e})", path.display());
                continue;
            }
        };
        stats.conversations += 1;

        let session_id = session_id_for(&messages, &conv_id);

        // Accumulate all edited code per file across this conversation.
        let mut file_code: BTreeMap<String, String> = BTreeMap::new();
        for msg in &messages {
            collect_edits(msg, &mut file_code);
        }

        for (raw_path, fragment_src) in &file_code {
            // Canonicalize before storing (CodeRabbit PR #279): the hook
            // (`post_tool_use`) and the co-edit backfill (`coedit_backfill`)
            // both store `canonical_repo_path` output as the path key; this
            // replay path must produce the same spelling or the attribution
            // join on `(name, file)` silently misses across writers.
            // Mirrors the hook's split exactly: the RAW path is the physical
            // file the transcript edited (read from disk below — a
            // linked-worktree file may differ from its main-checkout
            // counterpart), the CANONICAL path is only the storage key.
            let file_path = &crate::extraction::repo_path::canonical_repo_path(Path::new(raw_path))
                .to_string_lossy()
                .to_string();
            let lang = match lang_from_path_str(file_path) {
                Some(l) => l,
                None => {
                    stats.skipped_unsupported += 1;
                    // WP2 Stage 3 (H8 innovation, receipt R4): file-level
                    // provenance for a file the backfill SAW but can't
                    // parse, instead of it vanishing silently. Best-effort;
                    // never aborts the replay.
                    if !dry_run {
                        if let Err(e) = storage.mark_code_file_unsupported(project_name, file_path)
                        {
                            eprintln!(
                                "CSR backfill: ast_status=unsupported record error for {file_path} ({e})"
                            );
                        }
                    }
                    continue;
                }
            };

            // Prefer the complete on-disk file (resolves callers↔callees);
            // fall back to the concatenated edit fragment only if it's gone.
            // Read the RAW path — the physical file the session edited —
            // like the hook does; the canonical form may point at the main
            // checkout whose content differs from the worktree branch.
            let disk = disk_source(raw_path, &mut disk_cache);
            let source: &str = disk.as_deref().unwrap_or(fragment_src);
            if disk.is_some() {
                from_disk_files.insert((project_name.clone(), file_path.clone()));
            }

            let fragment = extract_graph_fragment(
                source,
                lang,
                file_path,
                project_name, // repo
                project_name, // project
                &conv_id,
                &session_id,
            );

            // Repo identity (WP2 Stage 1, H8 finding): stable across
            // cwd/session boundaries, unlike `project_name` — never
            // overwrites it. `repo_root::repo_root_for_file` caches
            // per-directory in-process, so this is cheap across the
            // (frequently repeated) directories in a large replay.
            let repo_root = crate::extraction::repo_root::repo_root_for_file(file_path);

            // Nodes: upsert as we go (oldest-first preserves first_conv_id).
            for node in &fragment.nodes {
                if !dry_run {
                    let mut node = node.clone();
                    node.repo_root = repo_root.clone();
                    if let Err(e) = storage.upsert_code_node(&node) {
                        eprintln!("CSR backfill: node upsert error for {file_path} ({e})");
                    }
                }
                stats.nodes_upserted += 1;
            }

            // Edges: newest conversation wins — overwrite the per-file entry.
            let key = (project_name.clone(), file_path.clone());
            latest_edges.insert(key.clone(), fragment.edges);
            latest_hash.insert(key, crate::extraction::anchors::hash_normalized(source));
            touched_projects.insert(project_name.clone());
        }

        if (i + 1) % PROGRESS_EVERY == 0 {
            eprintln!(
                "CSR backfill: {}/{} conversations processed ({} nodes so far)",
                i + 1,
                total_files,
                stats.nodes_upserted
            );
        }
    }

    stats.code_files = latest_edges.len();
    stats.edges_written = latest_edges.values().map(|e| e.len()).sum();
    stats.projects = touched_projects.len();
    stats.files_from_disk = from_disk_files.len();

    if !dry_run {
        // 3. Flush the newest edge set + file state per file.
        for ((project, file), edges) in &latest_edges {
            if let Err(e) = storage.replace_code_file_edges(project, file, edges) {
                eprintln!("CSR backfill: edge replace error for {file} ({e})");
            }
            if let Some(hash) = latest_hash.get(&(project.clone(), file.clone())) {
                if let Err(e) = storage.upsert_code_file_state(project, file, hash, false) {
                    eprintln!("CSR backfill: file state error for {file} ({e})");
                }
            }
        }

        // 4. Resolve placeholders + recompute rank per project.
        let mut total_placeholders = 0usize;
        let mut resolved = 0usize;
        for project in &touched_projects {
            match storage.resolve_code_edges(project) {
                Ok(rs) => {
                    total_placeholders += rs.total;
                    resolved += rs.resolved;
                }
                Err(e) => eprintln!("CSR backfill: resolve error for {project} ({e})"),
            }
            if let Err(e) = storage.compute_code_rank(project) {
                eprintln!("CSR backfill: rank error for {project} ({e})");
            }
        }
        stats.resolution_rate = if total_placeholders == 0 {
            0.0
        } else {
            resolved as f64 / total_placeholders as f64
        };
    }

    Ok(stats)
}

/// Outcome of the seq/is_sidechain backfill (Saga Phase 1 WS1).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SagaBackfillStats {
    /// Rows in import_state examined.
    pub files_checked: usize,
    /// import_state rows whose JSONL is no longer on disk (skipped, never silent).
    pub files_missing: usize,
    /// chunks rows updated with seq/is_sidechain.
    pub chunks_updated: usize,
}

impl SagaBackfillStats {
    pub fn format_text(&self) -> String {
        format!(
            "CSR saga-columns backfill\n\
             ──────────────────────────\n\
             files checked  : {}\n\
             files missing  : {}\n\
             chunks updated : {}\n",
            self.files_checked, self.files_missing, self.chunks_updated,
        )
    }
}

/// Re-parse every JSONL still referenced in `import_state` and UPDATE its chunks' seq +
/// is_sidechain columns to match a fresh `parse_jsonl_file` pass (same chunker, same
/// deterministic UUIDv5 ids, so re-parsed chunk ids line up with existing rows). Files no
/// longer on disk are skipped and counted — never silently dropped.
pub fn backfill_saga_columns(engine: &Engine) -> Result<SagaBackfillStats> {
    let storage = engine.storage();
    let mut stats = SagaBackfillStats::default();
    let file_paths = storage.list_all_import_state_file_paths()?;
    for file_path in file_paths {
        stats.files_checked += 1;
        let path = Path::new(&file_path);
        if !path.exists() {
            stats.files_missing += 1;
            eprintln!("CSR saga backfill: skip missing {file_path}");
            continue;
        }
        let project_name = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| normalize_project_name(&n.to_string_lossy()))
            .unwrap_or_default();
        let chunks = match parse_jsonl_file(path, &project_name) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("CSR saga backfill: parse error {file_path} ({e})");
                continue;
            }
        };
        for chunk in &chunks {
            match storage.set_chunk_saga_columns(&chunk.id, chunk.seq, chunk.is_sidechain) {
                Ok(()) => stats.chunks_updated += 1,
                Err(e) => eprintln!("CSR saga backfill: update error for {} ({e})", chunk.id),
            }
        }
    }
    Ok(stats)
}

/// Result of a `codegraph backfill-repo-root` run (also used for `--dry-run`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RepoRootBackfillStats {
    /// Distinct `code_nodes.file` values that had no `repo_root` yet.
    pub node_files_checked: usize,
    /// Of those, how many resolved to a git repo root.
    pub node_files_resolved: usize,
    /// `code_nodes` rows updated (0 in dry-run).
    pub node_rows_updated: usize,
    /// Distinct `code_evolution.file_path` values that had no `repo_root` yet.
    pub evolution_files_checked: usize,
    /// Of those, how many resolved to a git repo root.
    pub evolution_files_resolved: usize,
    /// `code_evolution` rows updated (0 in dry-run).
    pub evolution_rows_updated: usize,
}

impl RepoRootBackfillStats {
    pub fn format_text(&self, dry_run: bool) -> String {
        let mode = if dry_run { " (dry-run, no writes)" } else { "" };
        format!(
            "CSR repo_root backfill{mode}\n\
             ────────────────────────────\n\
             code_nodes:     files checked {}, resolved {}, rows updated {}\n\
             code_evolution: files checked {}, resolved {}, rows updated {}\n",
            self.node_files_checked,
            self.node_files_resolved,
            self.node_rows_updated,
            self.evolution_files_checked,
            self.evolution_files_resolved,
            self.evolution_rows_updated,
        )
    }
}

/// Backfill `repo_root` (WP2 Stage 1, H8 finding — receipt R4) for every
/// existing `code_nodes` / `code_evolution` row that predates the column.
/// Purely additive: only ever sets a currently-NULL `repo_root`, never
/// touches any other column, and is safe to re-run (idempotent — a second
/// pass finds nothing left to update).
///
/// Resolution is per DISTINCT file path (not per row) via
/// `extraction::repo_root::repo_root_for_file`, which itself tries `git -C
/// <dir> rev-parse --show-toplevel` first and falls back to an ancestor
/// `.git`-directory walk for files no longer on disk — exactly the fallback
/// this backfill needs, since most historically-recorded files are still
/// present but some are long gone (renamed/deleted since indexed).
pub fn backfill_repo_root(engine: &Engine, dry_run: bool) -> Result<RepoRootBackfillStats> {
    let storage = engine.storage();
    let mut stats = RepoRootBackfillStats::default();

    let node_files = storage.code_node_files_missing_repo_root()?;
    stats.node_files_checked = node_files.len();
    for file in &node_files {
        if let Some(root) = crate::extraction::repo_root::repo_root_for_file(file) {
            stats.node_files_resolved += 1;
            if !dry_run {
                match storage.set_repo_root_for_file(file, &root) {
                    Ok(n) => stats.node_rows_updated += n,
                    Err(e) => eprintln!(
                        "CSR repo_root backfill: code_nodes update error for {file} ({e})"
                    ),
                }
            }
        }
    }

    let evolution_files = storage.code_evolution_files_missing_repo_root()?;
    stats.evolution_files_checked = evolution_files.len();
    for file in &evolution_files {
        if let Some(root) = crate::extraction::repo_root::repo_root_for_file(file) {
            stats.evolution_files_resolved += 1;
            if !dry_run {
                match storage.set_repo_root_for_evolution_file(file, &root) {
                    Ok(n) => stats.evolution_rows_updated += n,
                    Err(e) => eprintln!(
                        "CSR repo_root backfill: code_evolution update error for {file} ({e})"
                    ),
                }
            }
        }
    }

    Ok(stats)
}

/// Result of a `codegraph backfill-attribution` run (also used for `--dry-run`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AttributionBackfillStats {
    /// `code_nodes` rows examined.
    pub nodes_checked: usize,
    /// Transcript-channel rows written (matched an earliest `code_evolution` event).
    pub transcript_attributed: usize,
    /// Rows skipped for the git channel because they have no blameable span:
    /// negative/inverted spans, or the module sentinel (kind='module', 0/0).
    pub git_invalid_span: usize,
    /// Rows skipped because the file no longer exists on disk.
    pub git_file_missing: usize,
    /// Rows skipped because no git repo root (or relative-path) could be resolved.
    pub git_no_repo: usize,
    /// Rows where `git log -L` ran but returned no introducing commit.
    pub git_no_commit: usize,
    /// Git-channel rows written.
    pub git_attributed: usize,
}

impl AttributionBackfillStats {
    pub fn format_text(&self, dry_run: bool) -> String {
        let mode = if dry_run { " (dry-run, no writes)" } else { "" };
        format!(
            "CSR attribution backfill{mode}\n\
             ──────────────────────────────\n\
             nodes checked          : {}\n\
             transcript attributed  : {}\n\
             git attributed         : {}\n\
             git skipped: invalid_span={}  file_missing={}  no_repo={}  no_commit={}\n",
            self.nodes_checked,
            self.transcript_attributed,
            self.git_attributed,
            self.git_invalid_span,
            self.git_file_missing,
            self.git_no_repo,
            self.git_no_commit,
        )
    }
}

/// Parse a `code_evolution.functions_added`/`types_added` JSON string array;
/// empty vec on any parse error (never fatal — matches the rest of this
/// module's fail-soft posture).
fn parse_evolution_names(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json).unwrap_or_default()
}

/// Build the transcript-channel index: `(file_path, symbol_name) -> (session_id,
/// timestamp)` of the EARLIEST `code_evolution` event naming that symbol
/// (H4 remediation, receipt R2 — "earliest by (timestamp, rowid)"). `events`
/// must already be ordered oldest-first (`Storage::all_code_evolution_events_ordered`);
/// `HashMap::entry().or_insert()` then keeps only the first (= earliest) sighting.
fn build_transcript_index(
    events: &[crate::storage::queries::CodeEvolutionEventRow],
) -> BTreeMap<(String, String), (String, String)> {
    let mut index: BTreeMap<(String, String), (String, String)> = BTreeMap::new();
    for (_rowid, session_id, file_path, timestamp, functions_added, types_added) in events {
        let names = parse_evolution_names(functions_added)
            .into_iter()
            .chain(parse_evolution_names(types_added));
        for name in names {
            index
                .entry((file_path.clone(), name))
                .or_insert_with(|| (session_id.clone(), timestamp.clone()));
        }
    }
    index
}

/// Resolve `file`'s path relative to `repo_root`. `None` when `file` is not
/// actually rooted under `repo_root` (e.g. a stale/mismatched `repo_root`
/// value) — never a guess.
fn relpath_in_repo(repo_root: &str, file: &str) -> Option<String> {
    Path::new(file)
        .strip_prefix(Path::new(repo_root))
        .ok()
        .map(|p| p.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
}

/// `git -C <repo_root> log -L<start>,<end>:<relpath> --format='%H %cI' --reverse
/// --no-patch`, first output line only. `start`/`end` are 1-based line numbers
/// (git's `-L` convention — `code_nodes.span_start`/`span_end` are 0-based, so
/// callers must pass `span_start + 1` / `span_end + 1`).
///
/// CRITICAL (receipt R8): never add `-1` / `-n` here. `git log -n 1 -L… --reverse`
/// applies `-n` BEFORE `--reverse` and returns the NEWEST commit, not the
/// introducing one — the exact trap this backfill exists to avoid. Reading only
/// the first line of the full (un-limited) `--reverse` output is what makes this
/// correct: the oldest commit touching the range is genuinely first.
///
/// `None` on any failure — no git repo at `repo_root`, `git` missing, the range
/// has no history, non-UTF8 output, malformed output. Never a guess.
fn git_log_introducing_commit(
    repo_root: &str,
    relpath: &str,
    start: i64,
    end: i64,
) -> Option<(String, String)> {
    if start < 1 || end < start {
        return None;
    }
    let range_arg = format!("-L{start},{end}:{relpath}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("log")
        .arg(&range_arg)
        .arg("--format=%H %cI")
        .arg("--reverse")
        .arg("--no-patch")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let first_line = text.lines().next()?.trim();
    if first_line.is_empty() {
        return None;
    }
    let (hash, ts) = first_line.split_once(' ')?;
    if hash.is_empty() || ts.is_empty() {
        return None;
    }
    Some((hash.to_string(), ts.to_string()))
}

/// Backfill `code_node_attribution` (WP2 Stage 2, H4 remediation fed by
/// H5/H6 — receipts R2/R3/R8) for every existing `code_nodes` row.
///
/// Two independent passes, never merged:
/// - **transcript**: joins each symbol (by name + file) against the
///   earliest `code_evolution` event that names it (`build_transcript_index`).
/// - **git**: for symbols with a blameable span (0-based, non-inverted,
///   excluding the kind='module' 0/0 sentinel) whose file
///   still exists on disk inside a resolvable git repo, walks
///   `git log -L… --reverse` to find the introducing commit
///   (`git_log_introducing_commit` — see its doc comment for the `-1`/`-n` trap).
///
/// Fail-soft per symbol throughout: a lookup/command failure is logged and
/// skipped, never fatal, matching this module's existing posture
/// (`backfill_code_graph`, `backfill_repo_root`). Idempotent — re-running
/// simply recomputes and upserts the same (or freshly-discovered) rows.
pub fn backfill_attribution(engine: &Engine, dry_run: bool) -> Result<AttributionBackfillStats> {
    backfill_attribution_into(engine.storage(), dry_run)
}

/// Storage-level core of `backfill_attribution` (no embeddings needed —
/// keeps unit tests light, same split as `backfill_into` above).
fn backfill_attribution_into(storage: &Storage, dry_run: bool) -> Result<AttributionBackfillStats> {
    let mut stats = AttributionBackfillStats::default();

    let nodes = storage.all_code_nodes()?;
    stats.nodes_checked = nodes.len();

    // Transcript channel.
    let events = storage.all_code_evolution_events_ordered()?;
    let transcript_index = build_transcript_index(&events);

    for node in &nodes {
        let Some((session_id, ts)) = transcript_index.get(&(node.file.clone(), node.name.clone()))
        else {
            continue;
        };
        stats.transcript_attributed += 1;
        if dry_run {
            continue;
        }
        let row = crate::storage::codegraph::AttributionRow {
            node_id: node.id.clone(),
            channel: "transcript".to_string(),
            source_id: session_id.clone(),
            observed_ts: Some(ts.clone()),
            evidence: "coedit_event".to_string(),
        };
        if let Err(e) = storage.upsert_code_attribution(&row) {
            eprintln!(
                "CSR attribution backfill: transcript upsert error for {} ({e})",
                node.id
            );
        }
    }

    // Git channel.
    for node in &nodes {
        // `span_start` is 0-based (format/mod.rs renders `span_start + 1`),
        // so a real definition on line 1 of a file legitimately has
        // `span_start == 0` — common for JS/TS/Python files whose first line
        // is the definition. Only the module-sentinel shape (kind='module',
        // hardcoded 0/0 span covering no real lines) has no blameable span.
        let module_sentinel = node.kind == "module" && node.span_start == 0 && node.span_end == 0;
        if node.span_start < 0 || node.span_end < node.span_start || module_sentinel {
            stats.git_invalid_span += 1;
            continue;
        }
        if !Path::new(&node.file).is_file() {
            stats.git_file_missing += 1;
            continue;
        }
        let repo_root = node
            .repo_root
            .clone()
            .or_else(|| crate::extraction::repo_root::repo_root_for_file(&node.file));
        let Some(repo_root) = repo_root else {
            stats.git_no_repo += 1;
            eprintln!(
                "CSR attribution backfill: no repo root for {} (git skip)",
                node.file
            );
            continue;
        };
        let Some(relpath) = relpath_in_repo(&repo_root, &node.file) else {
            stats.git_no_repo += 1;
            eprintln!(
                "CSR attribution backfill: {} is not under resolved repo root {} (git skip)",
                node.file, repo_root
            );
            continue;
        };

        match git_log_introducing_commit(
            &repo_root,
            &relpath,
            node.span_start + 1,
            node.span_end + 1,
        ) {
            Some((hash, ts)) => {
                stats.git_attributed += 1;
                if dry_run {
                    continue;
                }
                let row = crate::storage::codegraph::AttributionRow {
                    node_id: node.id.clone(),
                    channel: "git".to_string(),
                    source_id: hash,
                    observed_ts: Some(ts),
                    evidence: "git_log_L".to_string(),
                };
                if let Err(e) = storage.upsert_code_attribution(&row) {
                    eprintln!(
                        "CSR attribution backfill: git upsert error for {} ({e})",
                        node.id
                    );
                }
            }
            None => {
                stats.git_no_commit += 1;
            }
        }
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write a synthetic JSONL transcript with one Edit tool_use under a
    /// Claude-style project dir, and return the projects-base path.
    fn synthetic_projects(tmp: &Path, jsonl_line: &str) -> PathBuf {
        let proj = tmp.join("-Users-me-projects-demo");
        std::fs::create_dir_all(&proj).unwrap();
        let mut f = std::fs::File::create(proj.join("conv-abc.jsonl")).unwrap();
        writeln!(f, "{jsonl_line}").unwrap();
        tmp.to_path_buf()
    }

    fn edit_line() -> String {
        serde_json::json!({
            "type": "assistant",
            "sessionId": "sess-1",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "name": "Edit",
                    "input": {
                        "file_path": "/Users/me/projects/demo/src/lib.rs",
                        "old_string": "",
                        "new_string": "fn foo() {\n    bar();\n}\nfn bar() {}\n"
                    }
                }]
            }
        })
        .to_string()
    }

    #[test]
    fn collect_edits_extracts_edit_write_multiedit() {
        let msg = serde_json::json!({
            "message": {"content": [
                {"type": "tool_use", "name": "Edit",
                 "input": {"file_path": "a.rs", "new_string": "fn a() {}"}},
                {"type": "tool_use", "name": "Write",
                 "input": {"file_path": "b.rs", "content": "fn b() {}"}},
                {"type": "tool_use", "name": "MultiEdit",
                 "input": {"file_path": "a.rs", "edits": [
                     {"old_string": "x", "new_string": "fn c() {}"}]}},
                {"type": "tool_use", "name": "Read",
                 "input": {"file_path": "ignored.rs"}},
            ]}
        });
        let mut out = BTreeMap::new();
        collect_edits(&msg, &mut out);
        assert_eq!(out.len(), 2, "Read is ignored; a.rs + b.rs only");
        assert!(out["a.rs"].contains("fn a()"));
        assert!(out["a.rs"].contains("fn c()"), "MultiEdit appends to a.rs");
        assert!(out["b.rs"].contains("fn b()"));
    }

    #[test]
    fn backfill_extracts_nodes_and_edges_from_jsonl_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let projects_dir = synthetic_projects(tmp.path(), &edit_line());
        let storage = Storage::open_memory().unwrap();

        let stats = backfill_into(&storage, &projects_dir, false).unwrap();

        assert_eq!(stats.files_scanned, 1);
        assert_eq!(stats.conversations, 1);
        assert_eq!(stats.code_files, 1);
        assert!(stats.nodes_upserted >= 3, "module + foo + bar");
        assert!(stats.edges_written >= 1, "at least the foo->bar calls edge");
        assert_eq!(stats.projects, 1);

        // The graph is actually persisted: foo calls bar, resolvable.
        let foo_id = crate::extraction::codegraph::node_id(
            "demo",
            "/Users/me/projects/demo/src/lib.rs",
            "function",
            "foo",
        );
        let callees = storage.code_query_callees(&foo_id, 10).unwrap();
        assert!(
            callees.iter().any(|n| n.name == "bar"),
            "foo should call bar after backfill; got {:?}",
            callees.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn backfill_prefers_disk_file_over_fragment() {
        // JSONL records only a PARTIAL edit (just `foo`), but the real file on
        // disk has the whole module (foo -> bar, plus baz). Backfill must use
        // the disk copy so `bar`/`baz` exist and the call edge resolves.
        let tmp = tempfile::tempdir().unwrap();

        // Real source file on disk.
        let src_dir = tmp.path().join("realsrc");
        std::fs::create_dir_all(&src_dir).unwrap();
        let real_file = src_dir.join("lib.rs");
        std::fs::write(
            &real_file,
            "fn foo() {\n    bar();\n}\nfn bar() {}\nfn baz() {}\n",
        )
        .unwrap();
        let real_path = real_file.to_string_lossy().to_string();

        // JSONL transcript whose Edit fragment only mentions `foo` (no bar/baz).
        let proj = tmp.path().join("-Users-me-projects-demo");
        std::fs::create_dir_all(&proj).unwrap();
        let line = serde_json::json!({
            "type": "assistant",
            "sessionId": "sess-1",
            "message": {"content": [{
                "type": "tool_use",
                "name": "Edit",
                "input": {
                    "file_path": real_path,
                    "old_string": "",
                    "new_string": "fn foo() {\n    bar();\n}\n"
                }
            }]}
        })
        .to_string();
        let mut f = std::fs::File::create(proj.join("conv-disk.jsonl")).unwrap();
        writeln!(f, "{line}").unwrap();

        let storage = Storage::open_memory().unwrap();
        let stats = backfill_into(&storage, tmp.path(), false).unwrap();

        assert_eq!(stats.files_from_disk, 1, "the real file came from disk");

        // `baz` exists ONLY in the disk file, never in the JSONL fragment.
        let baz = storage.code_nodes_by_name("baz", "", 10).unwrap();
        assert!(!baz.is_empty(), "baz recovered from disk, not the fragment");

        // foo -> bar resolves because both defs are present (full file).
        let foo_id = crate::extraction::codegraph::node_id("demo", &real_path, "function", "foo");
        let callees = storage.code_query_callees(&foo_id, 10).unwrap();
        assert!(
            callees.iter().any(|n| n.name == "bar"),
            "foo->bar resolved from disk file; got {:?}",
            callees.iter().map(|n| &n.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn dry_run_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let projects_dir = synthetic_projects(tmp.path(), &edit_line());
        let storage = Storage::open_memory().unwrap();

        let stats = backfill_into(&storage, &projects_dir, true).unwrap();
        // Counting still happens.
        assert!(stats.nodes_upserted >= 3);
        assert_eq!(stats.conversations, 1);
        assert_eq!(stats.resolution_rate, 0.0, "no resolve in dry-run");

        // But nothing was persisted: no `foo` node exists in the graph.
        let nodes = storage.code_nodes_by_name("foo", "", 10).unwrap();
        assert!(nodes.is_empty(), "dry-run must not write code_nodes");
    }

    // ─── WP2 Stage 2: attribution backfill ───

    use crate::storage::codegraph::{upsert_node as codegraph_upsert_node, NodeRow};

    fn attr_node(id: &str, file: &str, name: &str) -> NodeRow {
        NodeRow {
            id: id.into(),
            repo: "repo".into(),
            project: "proj".into(),
            file: file.into(),
            lang: "rust".into(),
            kind: "function".into(),
            name: name.into(),
            first_conv_id: "legacy".into(),
            last_conv_id: "legacy".into(),
            ..NodeRow::default()
        }
    }

    /// Insert a `code_evolution` row via the real production path
    /// (`queries::insert_code_evolution`, timestamp = `Utc::now()` at call
    /// time), acquiring the storage mutex itself. Sequential calls therefore
    /// get non-decreasing timestamps, exactly like the live hook.
    fn insert_evolution_event(
        storage: &Storage,
        session_id: &str,
        file_path: &str,
        functions_added: &str,
        types_added: &str,
    ) {
        storage
            .with_connection(|conn| {
                crate::storage::queries::insert_code_evolution(
                    conn,
                    session_id,
                    "proj",
                    file_path,
                    "rust",
                    "Edit",
                    functions_added,
                    "[]",
                    types_added,
                    "[]",
                    "[]",
                    "[]",
                    None,
                )
            })
            .unwrap();
    }

    #[test]
    fn transcript_channel_picks_the_earliest_event_not_the_latest() {
        // Fixture for the earliest-event join (H4 remediation, receipt R2):
        // two code_evolution events name the same symbol in the same file —
        // the EARLIER one must win the transcript attribution, never the
        // later one.
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                codegraph_upsert_node(conn, &attr_node("n1", "a.rs", "foo")).unwrap();
                Ok(())
            })
            .unwrap();

        insert_evolution_event(&storage, "sess-older", "a.rs", "[\"foo\"]", "[]");
        insert_evolution_event(&storage, "sess-newer", "a.rs", "[\"foo\"]", "[]");

        let stats = backfill_attribution_into(&storage, false).unwrap();
        assert_eq!(stats.transcript_attributed, 1);

        let rows = storage.code_attribution_rows("n1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].channel, "transcript");
        assert_eq!(
            rows[0].source_id, "sess-older",
            "earliest-inserted event must win, not the later one: {rows:?}"
        );
    }

    #[test]
    fn transcript_channel_join_requires_matching_file_and_name() {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                codegraph_upsert_node(conn, &attr_node("n1", "a.rs", "foo")).unwrap();
                Ok(())
            })
            .unwrap();
        // Names "foo", but in a DIFFERENT file — must not attribute a.rs's foo.
        insert_evolution_event(&storage, "sess-1", "b.rs", "[\"foo\"]", "[]");

        let stats = backfill_attribution_into(&storage, false).unwrap();
        assert_eq!(
            stats.transcript_attributed, 0,
            "event naming 'foo' in a DIFFERENT file must not attribute a.rs's foo"
        );
        assert_eq!(
            storage.code_attribution_for_node("n1").unwrap(),
            "unattributed"
        );
    }

    #[test]
    fn nodes_with_no_matching_event_are_unattributed_not_falling_back_to_first_conv_id() {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                codegraph_upsert_node(conn, &attr_node("n1", "a.rs", "orphan")).unwrap();
                Ok(())
            })
            .unwrap();

        let stats = backfill_attribution_into(&storage, false).unwrap();
        assert_eq!(stats.transcript_attributed, 0);
        assert_eq!(stats.nodes_checked, 1);
        assert_eq!(
            storage.code_attribution_for_node("n1").unwrap(),
            "unattributed",
            "no event and no git span must never fall back to first_conv_id"
        );
    }

    #[test]
    fn dry_run_attribution_backfill_writes_nothing() {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                codegraph_upsert_node(conn, &attr_node("n1", "a.rs", "foo")).unwrap();
                crate::storage::queries::insert_code_evolution(
                    conn,
                    "sess-1",
                    "proj",
                    "a.rs",
                    "rust",
                    "Edit",
                    "[\"foo\"]",
                    "[]",
                    "[]",
                    "[]",
                    "[]",
                    "[]",
                    None,
                )
                .unwrap();
                Ok(())
            })
            .unwrap();

        let stats = backfill_attribution_into(&storage, true).unwrap();
        assert_eq!(stats.transcript_attributed, 1, "counting still happens");
        assert_eq!(
            storage.code_attribution_for_node("n1").unwrap(),
            "unattributed",
            "dry-run must not write code_node_attribution"
        );
    }

    #[test]
    fn git_channel_skips_module_sentinel_but_not_line_one_symbols() {
        // CodeRabbit PR #279: `span_start` is 0-based, so a real symbol
        // defined on line 1 also has `span_start == 0`. Only the module
        // sentinel (kind='module', 0/0 span) is span-invalid; a line-1
        // function must proceed past the span gate (here it then falls to
        // git_file_missing, proving it was NOT counted as invalid span).
        let storage = Storage::open_memory().unwrap();
        let mut sentinel = attr_node("n1", "/nonexistent/a.rs", "a.rs");
        sentinel.kind = "module".into();
        sentinel.span_start = 0;
        sentinel.span_end = 0;
        let mut line_one_fn = attr_node("n2", "/nonexistent/b.rs", "foo");
        line_one_fn.span_start = 0;
        line_one_fn.span_end = 3;
        storage
            .with_connection(|conn| {
                codegraph_upsert_node(conn, &sentinel).unwrap();
                codegraph_upsert_node(conn, &line_one_fn).unwrap();
                Ok(())
            })
            .unwrap();

        let stats = backfill_attribution_into(&storage, false).unwrap();
        assert_eq!(stats.git_invalid_span, 1, "only the module sentinel");
        assert_eq!(stats.git_file_missing, 1, "line-1 fn passed the span gate");
        assert_eq!(stats.git_attributed, 0);
    }

    #[test]
    fn git_log_introducing_commit_returns_the_oldest_commit_not_the_newest() {
        // R8's exact trap: a naive `-1 --reverse` returns the NEWEST commit
        // because `-n`/`-1` applies before `--reverse`. This test creates two
        // commits touching the same line and asserts the FIRST (oldest) one
        // is returned.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let git = |args: &[&str]| Command::new("git").arg("-C").arg(repo).args(args).status();
        if git(&["init", "-q"]).map(|s| !s.success()).unwrap_or(true) {
            return; // git unavailable — fail-soft skip, matches repo_root.rs precedent
        }
        git(&["config", "user.email", "t@example.com"]).unwrap();
        git(&["config", "user.name", "Test"]).unwrap();

        let file = repo.join("lib.rs");
        std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
        git(&["add", "lib.rs"]).unwrap();
        git(&["commit", "-q", "-m", "introduce foo"]).unwrap();
        let oldest_hash = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        // A second commit that also touches line 2 (same range).
        std::fs::write(&file, "fn foo() {\n    2\n}\n").unwrap();
        git(&["commit", "-q", "-am", "tweak foo"]).unwrap();

        let got = git_log_introducing_commit(&repo.to_string_lossy(), "lib.rs", 1, 3);
        let (hash, _ts) = got.expect("introducing commit must resolve");
        assert_eq!(
            hash, oldest_hash,
            "must return the OLDEST commit touching the range, not the newest"
        );
    }
}
