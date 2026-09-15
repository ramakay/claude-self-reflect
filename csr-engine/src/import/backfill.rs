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
use uuid::Uuid;

use crate::engine::Engine;
use crate::extraction::ast_analysis::lang_from_path_str;
use crate::extraction::codegraph::{
    container_spans, extract_graph_fragment, extract_graph_fragment_for_file, ContainerSpan,
};
use crate::import::{
    discover_projects, list_jsonl_files, normalize_project_name, parse_jsonl_file,
    parse_jsonl_messages,
};
use crate::storage::codegraph::{EdgeRow, NodeRow};
use crate::storage::witness_ledger::{self, WitnessLedgerRow, WITNESS_EXTRACTOR_VERSION};
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

    // Newest-wins per-file edge set + content hash + node ids (flushed once at the end).
    let mut latest_edges: BTreeMap<(String, String), Vec<EdgeRow>> = BTreeMap::new();
    let mut latest_hash: BTreeMap<(String, String), String> = BTreeMap::new();
    let mut latest_node_ids: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
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

            // Edges + node ids: newest conversation wins — overwrite the per-file entry.
            let key = (project_name.clone(), file_path.clone());
            latest_edges.insert(key.clone(), fragment.edges);
            latest_node_ids.insert(
                key.clone(),
                fragment.nodes.iter().map(|n| n.id.clone()).collect(),
            );
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
        // 3. Flush the newest edge set + file state + retire stale nodes per file.
        for ((project, file), edges) in &latest_edges {
            if let Err(e) = storage.replace_code_file_edges(project, file, edges) {
                eprintln!("CSR backfill: edge replace error for {file} ({e})");
            }
            if let Some(hash) = latest_hash.get(&(project.clone(), file.clone())) {
                if let Err(e) = storage.upsert_code_file_state(project, file, hash, false) {
                    eprintln!("CSR backfill: file state error for {file} ({e})");
                }
            }
            // Retire ONLY from a complete on-disk observation. When the file is
            // gone — routine for a pruned worktree — extraction above falls back
            // to concatenated transcript edit snippets, which are fragments of a
            // file, not a file. Their node ids are non-empty, so the empty-set
            // guard in `retire_missing_nodes` does not help: every historical
            // symbol the snippet happens not to contain would be hard-deleted
            // along with its `code_node_attribution` provenance. Edge replacement
            // and file state above are keyed to the same observation, but only
            // retirement destroys history, so only it is gated here.
            if from_disk_files.contains(&(project.clone(), file.clone())) {
                if let Some(ids) = latest_node_ids.get(&(project.clone(), file.clone())) {
                    if let Err(e) = storage.retire_missing_code_nodes(project, file, ids) {
                        eprintln!("CSR backfill: node retire error for {file} ({e})");
                    }
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

/// A `git -C <repo_root>` command with any ambient `GIT_*` environment
/// stripped. When the calling process itself runs inside a git hook (git
/// exports `GIT_DIR`/`GIT_INDEX_FILE`/... to hooks — absolute paths in a
/// linked worktree), an inherited `GIT_DIR` would silently override `-C`
/// and point the command at the WRONG repository. This backfill always
/// targets the explicit `repo_root` it resolved — never ambient state.
fn git_at(repo_root: &str) -> Command {
    let mut cmd = Command::new("git");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(&k);
        }
    }
    cmd.arg("-C").arg(repo_root);
    cmd
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
    let output = git_at(repo_root)
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

/// Result of a `codegraph stamp-spans` run (also used for `--dry-run`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StampSpansStats {
    /// Distinct files seen in `code_nodes` that resolved to a repo_root.
    pub files_checked: usize,
    /// Of those, how many were actually stamped (file on disk, repo
    /// discoverable, HEAD resolvable, path relative to its own repo_root).
    pub files_processed: usize,
    /// Function/type/const-level span stamps successfully computed. A failed
    /// sibling prevents the entire re-derived file generation from publishing,
    /// so this can exceed the number of ledger rows inserted by a partial run.
    pub spans_stamped: usize,
    /// Whole-file witnesses minted (`symbol` NULL) — only for files where
    /// span extraction found no function/type/const-level node.
    pub whole_files_stamped: usize,
    /// No `repo_root` could be resolved for the file (neither stored on any
    /// of its `code_nodes` rows nor re-derivable live) — non-git file, or a
    /// repo that can no longer be found.
    pub skipped_no_repo_root: usize,
    /// The file no longer exists on disk.
    pub skipped_file_missing: usize,
    /// `repo_root` does not open/discover as a git repository, or has no
    /// resolvable HEAD (empty/unborn repo).
    pub skipped_non_git: usize,
    /// The file's path does not resolve to a path relative to its own
    /// `repo_root` (a stale/mismatched `repo_root` value — never a guess).
    pub skipped_outside_repo_root: usize,
    /// `codewitness::Auditor::stamp_at` itself failed for a specific anchor
    /// (e.g. the anchor's content vanished from the commit tree between
    /// extraction time and stamping time).
    pub skipped_stamp_error: usize,
    /// The node's 0-based span could not be converted to the 1-based `u32`
    /// line range `codewitness::Anchor` expects (overflow on `+1` or value
    /// outside `u32`) — corrupt span data, skipped rather than panicking
    /// (debug) or truncating (release).
    pub skipped_span_out_of_range: usize,
    /// Historical mode (`--at <rev>`) only: `rev` did not resolve to a
    /// commit in a given repo (wrong repo, typo'd SHA, a rev that only
    /// exists in a sibling repository) — that repo is skipped entirely.
    /// Always 0 in HEAD-tracking mode.
    pub skipped_rev_unresolved: usize,
    /// Historical mode only: a blob at the historical commit was not valid
    /// UTF-8, so it cannot be handed to the (text-based) span extractor.
    /// Always 0 in HEAD-tracking mode (which never decodes file content —
    /// `codewitness` hashes raw bytes directly).
    pub skipped_non_utf8: usize,
    /// Historical mode only: `(repo_root, resolved commit oid)` for every
    /// repo actually visited — printed as the `at_commit:` line(s) in
    /// [`Self::format_text`]. Empty in HEAD-tracking mode.
    pub at_commits: Vec<(String, String)>,
    /// Symbol-identity collision safety net (type-qualified symbol
    /// identity fix): a `witness_ledger` row whose qualified symbol still
    /// collided with an earlier row from the SAME `(file, at_oid)` batch
    /// after container qualification (see `qualify_witness_symbols`'s doc
    /// comment) and was disambiguated with a deterministic `#2`/`#3`/...
    /// suffix rather than silently joining. Should be rare in practice —
    /// container qualification resolves the overwhelming majority of
    /// same-name collisions (e.g. `is_empty` in two different `impl`
    /// blocks) on its own.
    pub disambiguated_symbols: usize,
    /// Files skipped before blob loading/parsing/stamping because a complete
    /// generation already exists for the same HEAD and extractor version.
    pub skipped_complete_generation: usize,
}

impl StampSpansStats {
    /// Human-readable one-block summary.
    pub fn format_text(&self, dry_run: bool) -> String {
        let mode = if dry_run { " (dry-run, no writes)" } else { "" };
        let mut out = format!(
            "CSR stamp-spans backfill{mode}\n\
             ──────────────────────────────\n\
             files checked        : {}\n\
             files processed      : {}\n\
             spans stamped        : {}\n\
             whole-file witnesses : {}\n\
             skipped: no_repo_root={}  file_missing={}  non_git={}  outside_repo_root={}  stamp_error={}  span_out_of_range={}  rev_unresolved={}  non_utf8={}\n\
             disambiguated symbols : {}\n\
             skipped complete generation : {}\n",
            self.files_checked,
            self.files_processed,
            self.spans_stamped,
            self.whole_files_stamped,
            self.skipped_no_repo_root,
            self.skipped_file_missing,
            self.skipped_non_git,
            self.skipped_outside_repo_root,
            self.skipped_stamp_error,
            self.skipped_span_out_of_range,
            self.skipped_rev_unresolved,
            self.skipped_non_utf8,
            self.disambiguated_symbols,
            self.skipped_complete_generation,
        );
        // Single repo (the common case, especially with `--repo`): the
        // exact `at_commit: <oid>` form. Multiple repos (the `--at`
        // default of "every known root"): one prefixed line per repo, still
        // grep-able on the `at_commit:` prefix.
        match self.at_commits.as_slice() {
            [] => {}
            [(_, oid)] => out.push_str(&format!("at_commit: {oid}\n")),
            many => {
                for (repo_root, oid) in many {
                    out.push_str(&format!("at_commit[{repo_root}]: {oid}\n"));
                }
            }
        }
        out
    }
}

/// Type-qualify a file's function/method definitions for `witness_ledger`
/// symbol identity (adversarial-gate audit: `(file, kind, name)` extraction
/// identity — see `extraction::codegraph::node_id`'s doc comment — has no
/// receiver/type scoping, so two unrelated same-named methods in different
/// `impl`/class bodies join as if they were the same symbol across
/// commits). Returns, in the SAME order/length as `defs`, the symbol string
/// to mint for each definition — NEVER written back to `NodeRow::name` or
/// `code_nodes` (SCOPE DECISION: this is `witness_ledger`-only
/// disambiguation, shared byte-for-byte between [`stamp_spans_into`] (HEAD
/// mode) and [`stamp_spans_historical_into`] (`--at` mode) so a symbol
/// minted by one mode is joinable — by `(file, symbol)` — with the same
/// method's row minted by the other).
///
/// `defs` is `(kind, name, span_start, span_end)` per definition, matching
/// `NodeRow`'s own fields — one entry per node the caller is about to
/// consider stamping (module-kind sentinels included is harmless: `kind !=
/// "function"` leaves them bare, and they're skipped before insertion by
/// both callers anyway).
///
/// Qualification, in order:
/// 1. A `kind == "function"` node whose span is fully CONTAINED in an
///    impl/class container span (`containers`, from
///    `extraction::codegraph::container_spans`) is prefixed
///    `<Container><separator><name>`; the INNERMOST (smallest-span)
///    containing container wins if containers ever nest. Non-function kinds
///    (`type`, `const`) and functions with no containing container are left
///    bare.
/// 2. Collision safety net: qualification can still leave two definitions
///    on the identical string (a genuine duplicate/cfg-gated redefinition,
///    or a container this function couldn't name) — the 2nd/3rd/...
///    occurrence, by deterministic SPAN order (`(span_start, span_end,
///    name)` — never `defs`' own input order, which for both callers comes
///    from a hash-keyed source: `code_nodes.id`/`BTreeMap<node_id, _>`, not
///    source position), gets a `#2`/`#3`/... suffix. This guarantees no two
///    rows from the same `(file, at_oid)` batch ever mint an identical
///    `symbol` to `witness_ledger`, closing the collision even when static
///    qualification cannot.
///
/// Identity migration safety: this function is used only while minting new
/// append-only witness rows. Qualification changes intentionally fork anchor
/// lineages: existing rows keep dreaming under their old symbol, that old
/// lineage eventually goes quiet, and newly minted rows accumulate under the
/// new qualified symbol. Existing `witness_ledger` rows are never rewritten or
/// migrated to make the lineages appear continuous.
fn qualify_witness_symbols(
    defs: &[(String, String, i64, i64)],
    containers: &[ContainerSpan],
) -> (Vec<String>, usize) {
    let mut order: Vec<usize> = (0..defs.len()).collect();
    order.sort_by(|&a, &b| {
        let (_, na, sa, ea) = &defs[a];
        (sa, ea, na).cmp(&{
            let (_, nb, sb, eb) = &defs[b];
            (sb, eb, nb)
        })
    });

    let mut qualified = vec![String::new(); defs.len()];
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut disambiguated = 0usize;
    for i in order {
        let (kind, name, start, end) = &defs[i];
        let base = if kind == "function" {
            qualify_one_symbol(containers, *start, *end, name)
        } else {
            name.clone()
        };
        let count = seen.entry(base.clone()).or_insert(0);
        *count += 1;
        qualified[i] = if *count == 1 {
            base
        } else {
            disambiguated += 1;
            format!("{base}#{count}")
        };
    }
    (qualified, disambiguated)
}

/// Container-qualify a single definition's name by span containment —
/// the innermost (smallest-span) containing `ContainerSpan` wins when
/// containers nest.
fn qualify_one_symbol(containers: &[ContainerSpan], start: i64, end: i64, name: &str) -> String {
    let mut best: Option<&ContainerSpan> = None;
    for c in containers {
        // A named function/method is itself a container for nested defs, but
        // must not qualify its own symbol as `name.name`. Line spans are the
        // only persisted coordinates, so require proper (not equal) span
        // containment here.
        if c.start <= start && end <= c.end && (c.start < start || end < c.end) {
            let better = match best {
                None => true,
                Some(b) => (c.end - c.start) < (b.end - b.start),
            };
            if better {
                best = Some(c);
            }
        }
    }
    match best {
        Some(c) => format!("{}{}{}", c.name, c.separator, name),
        None => name.to_string(),
    }
}

/// Open (discover) the git repository at `repo_root` and resolve its HEAD
/// commit, once. `None` on any failure — not a git repository, `repo_root`
/// no longer exists, or the repo has no commits yet (unborn HEAD) — the
/// caller folds this into `skipped_non_git`, never a hard failure (matches
/// this module's existing fail-soft posture).
///
/// `pub(crate)`: `dream`'s successor join reuses this exact resolution
/// (rather than duplicating it) since it needs the identical live-HEAD
/// concept `stamp_spans_into` already establishes for a repo.
pub(crate) fn open_repo_head(
    repo_root: &str,
) -> Option<(codewitness::Auditor, codewitness::ObjectId)> {
    let auditor = codewitness::Auditor::discover(repo_root).ok()?;
    let head = auditor.repo().head_id().ok()?.detach();
    Some((auditor, head))
}

/// Backfill `witness_ledger` (v10 "dreaming" substrate — see
/// `storage::witness_ledger`'s module doc) with committed-tier
/// `codewitness` stamps for every function/type/const span the code graph
/// already knows about, plus a whole-file fallback witness for files whose
/// span extraction yielded nothing.
///
/// Reuses `code_nodes` exactly as `backfill_attribution_into`'s git channel
/// does: no new parser, no new AST pass — `code_nodes.span_start`/`span_end`
/// (0-based, tree-sitter row numbers; see `extraction::codegraph::extract_inner`)
/// are the SAME spans that channel already walks with `git log -L`, converted
/// here to the 1-based inclusive range `codewitness::Anchor::with_span`
/// expects (`+1`, matching `git_log_introducing_commit`'s own `span_start + 1`
/// convention).
///
/// `repo_root` resolution mirrors `backfill_attribution_into`'s git channel:
/// prefer the value already stored on the node, falling back to a live
/// `extraction::repo_root::repo_root_for_file` lookup so this backfill still
/// works on a DB that predates (or raced) `codegraph backfill-repo-root`.
///
/// Fail-soft per file throughout, mirroring the rest of this module: a
/// missing file, a non-git (or HEAD-less) repository, a path that doesn't
/// resolve under its own `repo_root`, or a specific anchor's stamp failing
/// is logged and counted in a reason bucket on `StampSpansStats`, never
/// fatal. Idempotent by construction: `Storage::insert_witness` is
/// `INSERT OR IGNORE` against the ledger's `idx_witness_ledger_identity`
/// UNIQUE index, whose `COALESCE`-normalized key columns dedupe symbol-level
/// AND whole-file (NULL-key) rows atomically at the DB level — see
/// `storage::witness_ledger`'s module doc.
pub fn backfill_stamp_spans(engine: &Engine, dry_run: bool) -> Result<StampSpansStats> {
    stamp_spans_into(engine.storage(), dry_run)
}

/// Cancellation-aware daemon variant. Checks between files and periodically
/// within files; only fully published per-file generations remain visible.
pub(crate) fn backfill_stamp_spans_cancellable(
    engine: &Engine,
    dry_run: bool,
    should_cancel: &dyn Fn() -> bool,
) -> Result<StampSpansStats> {
    stamp_spans_into_cancellable(engine.storage(), dry_run, Some(should_cancel))
}

/// Storage-level core of `backfill_stamp_spans` (no embeddings needed —
/// keeps unit tests light, same split as `backfill_into` / `backfill_attribution_into`).
fn stamp_spans_into(storage: &Storage, dry_run: bool) -> Result<StampSpansStats> {
    stamp_spans_into_cancellable(storage, dry_run, None)
}

fn stamp_spans_into_cancellable(
    storage: &Storage,
    dry_run: bool,
    should_cancel: Option<&dyn Fn() -> bool>,
) -> Result<StampSpansStats> {
    stamp_spans_into_cancellable_with(storage, dry_run, should_cancel, &|anchor, bytes| {
        Ok(codewitness::Auditor::stamp_file_content(anchor, bytes)?
            .as_str()
            .to_string())
    })
}

fn stamp_spans_into_cancellable_with(
    storage: &Storage,
    dry_run: bool,
    should_cancel: Option<&dyn Fn() -> bool>,
    stamp_cached: &dyn Fn(&codewitness::Anchor, &[u8]) -> Result<String>,
) -> Result<StampSpansStats> {
    let mut stats = StampSpansStats::default();

    // Group by file first: repo access (discover + HEAD resolution) and the
    // whole-file-witness decision both happen per file, not per node.
    let nodes = storage.all_code_nodes()?;
    let mut by_file: BTreeMap<String, Vec<NodeRow>> = BTreeMap::new();
    for node in nodes {
        by_file.entry(node.file.clone()).or_default().push(node);
    }

    // Cache Auditor + HEAD oid per repo_root — discovery + HEAD resolution
    // happen once per distinct repo, not once per file in that repo.
    let mut repo_cache: BTreeMap<String, Option<(codewitness::Auditor, codewitness::ObjectId)>> =
        BTreeMap::new();

    for (file, file_nodes) in by_file {
        if should_cancel.is_some_and(|cancel| cancel()) {
            break;
        }
        // Same fallback order as `backfill_attribution_into`'s git channel:
        // prefer a stored repo_root, else resolve live.
        let repo_root = file_nodes
            .iter()
            .find_map(|n| n.repo_root.clone())
            .or_else(|| crate::extraction::repo_root::repo_root_for_file(&file));
        let Some(repo_root) = repo_root else {
            stats.skipped_no_repo_root += 1;
            continue;
        };
        // Counted as soon as a repo_root resolves (the doc contract on
        // `StampSpansStats::files_checked`) — BEFORE the remaining skip
        // gates, so the summary's denominator includes skipped files and
        // `files_processed` can actually differ from it in HEAD mode.
        stats.files_checked += 1;

        if !Path::new(&file).is_file() {
            stats.skipped_file_missing += 1;
            continue;
        }

        let cached = repo_cache
            .entry(repo_root.clone())
            .or_insert_with(|| open_repo_head(&repo_root));
        let Some((auditor, head_oid)) = cached.as_ref() else {
            stats.skipped_non_git += 1;
            continue;
        };
        let head_oid = *head_oid;

        let Some(relpath) = relpath_in_repo(&repo_root, &file) else {
            stats.skipped_outside_repo_root += 1;
            eprintln!("CSR stamp-spans: {file} is not under resolved repo root {repo_root} (skip)");
            continue;
        };

        let project = file_nodes
            .iter()
            .find(|n| !n.project.is_empty())
            .map(|n| n.project.clone())
            .unwrap_or_default();
        let at_oid_str = head_oid.to_string();

        if !dry_run
            && storage.with_connection(|conn| {
                witness_ledger::complete_generation_exists(
                    conn,
                    &project,
                    &file,
                    &at_oid_str,
                    WITNESS_EXTRACTOR_VERSION,
                )
            })?
        {
            stats.skipped_complete_generation += 1;
            continue;
        }
        stats.files_processed += 1;

        // Re-derive CURRENT anchor occurrences from the production extractor
        // instead of trusting persisted `code_nodes`, whose coarse ids may
        // already have collapsed coexisting same-named definitions. Limit
        // the fresh fragment to `(kind,name)` anchors the graph actually
        // knows, so this repairs identity without inventing unrelated graph
        // coverage. A clean parse is required: on malformed/unreadable input
        // retain the legacy rows and do not mint a preferred lineage.
        // Read the exact committed blob `stamp_at(..., head_oid)` will hash.
        // Parsing the worktree while stamping HEAD would mix two evidence
        // versions under one source_id whenever the file has local edits.
        let source_bytes = match auditor.file_content_at(Path::new(&relpath), head_oid) {
            Ok(bytes) => bytes,
            Err(e) => {
                stats.skipped_stamp_error += 1;
                eprintln!("CSR stamp-spans: blob read error for {file} ({e})");
                continue;
            }
        };
        let source = std::str::from_utf8(&source_bytes).ok();
        let lang = lang_from_path_str(&file);
        let anchor_keys: BTreeSet<(String, String)> = file_nodes
            .iter()
            .filter(|node| node.kind != "module")
            .map(|node| (node.kind.clone(), node.name.clone()))
            .collect();
        let rederived = source.zip(lang).and_then(|(src, _)| {
            let fragment = extract_graph_fragment_for_file(
                src,
                &file,
                &repo_root,
                &project,
                &at_oid_str,
                &at_oid_str,
            );
            fragment.parse_clean.then(|| {
                let nodes = fragment
                    .nodes
                    .into_iter()
                    .filter(|node| {
                        node.kind != "module"
                            && anchor_keys.contains(&(node.kind.clone(), node.name.clone()))
                    })
                    .collect::<Vec<_>>();
                (nodes, fragment.containers)
            })
        });
        let (stamp_nodes, containers, source_kind, is_rederived): (
            Vec<NodeRow>,
            Vec<ContainerSpan>,
            &str,
            bool,
        ) = match rederived {
            Some((nodes, containers)) => (nodes, containers, "backfill_rederived_v2", true),
            None => (file_nodes.clone(), Vec::new(), "backfill", false),
        };
        let defs: Vec<(String, String, i64, i64)> = stamp_nodes
            .iter()
            .map(|n| (n.kind.clone(), n.name.clone(), n.span_start, n.span_end))
            .collect();
        let (qualified_symbols, disambiguated) = qualify_witness_symbols(&defs, &containers);
        stats.disambiguated_symbols += disambiguated;
        let attribution_node_ids: Vec<Option<String>> = stamp_nodes
            .iter()
            .map(|node| {
                if !is_rederived {
                    return Some(node.id.clone());
                }
                let exact: Vec<&NodeRow> = file_nodes
                    .iter()
                    .filter(|stored| {
                        stored.kind == node.kind
                            && stored.name == node.name
                            && stored.span_start == node.span_start
                            && stored.span_end == node.span_end
                    })
                    .collect();
                if exact.len() == 1 {
                    return Some(exact[0].id.clone());
                }
                let same_anchor: Vec<&NodeRow> = file_nodes
                    .iter()
                    .filter(|stored| stored.kind == node.kind && stored.name == node.name)
                    .collect();
                (same_anchor.len() == 1).then(|| same_anchor[0].id.clone())
            })
            .collect();

        let generation_id = Uuid::new_v4().to_string();
        let generation = |status: &str| witness_ledger::WitnessGeneration {
            id: 0,
            generation_id: generation_id.clone(),
            project: project.clone(),
            file: file.clone(),
            repo_root: Some(repo_root.clone()),
            head_oid: at_oid_str.clone(),
            extractor_version: WITNESS_EXTRACTOR_VERSION.to_string(),
            status: status.to_string(),
        };

        let mut any_span = false;
        let mut generation_failed = false;
        let mut pending_rows = Vec::new();
        for (idx, node) in stamp_nodes.iter().enumerate() {
            if idx % 64 == 0 && should_cancel.is_some_and(|cancel| cancel()) {
                return Ok(stats);
            }
            // The synthetic module sentinel (kind='module', 0/0 span) isn't a
            // real span-level symbol — same exclusion as the git-channel
            // attribution backfill's `module_sentinel` check.
            if node.kind == "module" {
                continue;
            }
            any_span = true;
            if node.span_start < 0 || node.span_end < node.span_start {
                generation_failed |= is_rederived;
                continue;
            }

            // Checked 0-based → 1-based conversion: a corrupt span near
            // i64::MAX (or one that simply doesn't fit u32) must be skipped
            // and counted, never panic (debug) or silently truncate (release).
            let checked_line = |span: i64| u32::try_from(span.checked_add(1)?).ok();
            let (Some(start_line), Some(end_line)) =
                (checked_line(node.span_start), checked_line(node.span_end))
            else {
                stats.skipped_span_out_of_range += 1;
                generation_failed |= is_rederived;
                eprintln!(
                    "CSR stamp-spans: span {}..{} for {file}::{} does not fit a 1-based u32 line (skip)",
                    node.span_start, node.span_end, node.name
                );
                continue;
            };
            let symbol = &qualified_symbols[idx];
            let anchor = codewitness::Anchor::new(relpath.clone())
                .with_symbol(symbol.clone())
                .with_span(start_line, end_line);

            match stamp_cached(&anchor, &source_bytes) {
                Ok(stamp) => {
                    stats.spans_stamped += 1;
                    if dry_run {
                        continue;
                    }
                    pending_rows.push((
                        WitnessLedgerRow {
                            id: 0,
                            project: project.clone(),
                            file: file.clone(),
                            symbol: Some(symbol.clone()),
                            span_start: Some(node.span_start),
                            span_end: Some(node.span_end),
                            stamp,
                            tier: "committed".to_string(),
                            at_oid: Some(at_oid_str.clone()),
                            source_kind: source_kind.to_string(),
                            source_id: Some(if is_rederived {
                                generation_id.clone()
                            } else {
                                at_oid_str.clone()
                            }),
                        },
                        attribution_node_ids[idx].clone(),
                    ));
                }
                Err(e) => {
                    stats.skipped_stamp_error += 1;
                    generation_failed |= is_rederived;
                    eprintln!(
                        "CSR stamp-spans: stamp error for {file}::{} ({e})",
                        node.name
                    );
                }
            }
        }

        if any_span {
            // A lineage generation is a FILE batch. Publish it atomically so
            // the binding read path can never observe a half-minted newest
            // generation and mistake one suffix candidate for a unique bind.
            if !dry_run && is_rederived && generation_failed {
                storage.with_connection(|conn| {
                    witness_ledger::insert_generation(conn, &generation("incomplete"))
                })?;
            } else if !dry_run && !pending_rows.is_empty() {
                if let Err(e) = storage.with_connection(|conn| {
                    let tx = conn.unchecked_transaction()?;
                    for (row, node_id) in &pending_rows {
                        witness_ledger::insert_witness(&tx, row)?;
                        if let Some(node_id) = node_id {
                            crate::storage::witness_verdicts::bind_witness_row_to_node_chunk(
                                &tx, row, node_id,
                            )?;
                        }
                    }
                    if is_rederived {
                        witness_ledger::insert_generation(&tx, &generation("complete"))?;
                    }
                    tx.commit()?;
                    Ok(())
                }) {
                    eprintln!("CSR stamp-spans: atomic ledger batch error for {file} ({e})");
                }
            }
            continue;
        }

        // No function/type/const span in this file — whole-file fallback
        // witness (symbol = NULL).
        let anchor = codewitness::Anchor::new(relpath.clone());
        match stamp_cached(&anchor, &source_bytes) {
            Ok(stamp) => {
                stats.whole_files_stamped += 1;
                if dry_run {
                    continue;
                }
                let row = WitnessLedgerRow {
                    id: 0,
                    project: project.clone(),
                    file: file.clone(),
                    symbol: None,
                    span_start: None,
                    span_end: None,
                    stamp,
                    tier: "committed".to_string(),
                    at_oid: Some(at_oid_str.clone()),
                    source_kind: source_kind.to_string(),
                    source_id: Some(if is_rederived {
                        generation_id.clone()
                    } else {
                        at_oid_str.clone()
                    }),
                };
                if let Err(e) = storage.with_connection(|conn| {
                    let tx = conn.unchecked_transaction()?;
                    witness_ledger::insert_witness(&tx, &row)?;
                    if is_rederived {
                        witness_ledger::insert_generation(&tx, &generation("complete"))?;
                    }
                    tx.commit()?;
                    Ok(())
                }) {
                    eprintln!("CSR stamp-spans: ledger insert error for {file} (whole-file) ({e})");
                }
            }
            Err(e) => {
                stats.skipped_stamp_error += 1;
                eprintln!("CSR stamp-spans: whole-file stamp error for {file} ({e})");
            }
        }
    }

    Ok(stats)
}

/// Historical counterpart to [`backfill_stamp_spans`]: mint `witness_ledger`
/// rows for function/type/const spans (plus whole-file fallback) AS THEY
/// EXISTED at `rev`, not at each repo's live HEAD — the substrate for the
/// S3 time-travel precision gate (v10 "dreaming").
///
/// `repo_filter`: when `Some`, restrict to that one repo root. `None` visits
/// every repo root already known to the code graph (mirrors
/// [`backfill_stamp_spans`]'s default footprint) — `rev` is resolved
/// independently in each one, and a repo where it doesn't resolve is
/// skipped (`skipped_rev_unresolved`), never fatal.
pub fn backfill_stamp_spans_at(
    engine: &Engine,
    rev: &str,
    repo_filter: Option<&str>,
    dry_run: bool,
) -> Result<StampSpansStats> {
    stamp_spans_historical_into(engine.storage(), rev, repo_filter, dry_run)
}

/// Storage-level core of [`backfill_stamp_spans_at`] (no embeddings needed,
/// same split as every other backfill in this module).
///
/// Unlike [`stamp_spans_into`] (which walks `code_nodes` — files the code
/// graph has already seen via JSONL replay or the live hook), this walks
/// the COMMIT'S OWN tree: a historical commit can contain files the graph
/// never saw, or omit files the graph still remembers, so the graph's node
/// list is the wrong source of "which files" here (see this function's
/// module-level design doc). `code_nodes` is consulted only to answer two
/// narrower questions: which repo roots to visit by default, and which
/// `project` tag to stamp each repo's rows with (the closest historical
/// mode can get to `stamp_spans_into`'s per-file JSONL-derived project,
/// since there is no JSONL context for a bare historical commit).
///
/// Extraction reuses the exact same text-based extractor
/// (`extraction::codegraph::extract_graph_fragment_for_file`) every other
/// write path uses, applied to blob content read at `rev` instead of a
/// JSONL fragment or an on-disk file — so `symbol` (`NodeRow::name`) carries
/// the identical convention across every mode, and rows minted from
/// different commits stay joinable by `(file, symbol)`.
fn stamp_spans_historical_into(
    storage: &Storage,
    rev: &str,
    repo_filter: Option<&str>,
    dry_run: bool,
) -> Result<StampSpansStats> {
    let mut stats = StampSpansStats::default();

    // Which repo roots to visit, and a representative `project` tag for
    // each — the first non-empty `code_nodes.project` seen for that root
    // (repo-level affinity; historical extraction has no per-file JSONL
    // context to draw a per-file project from the way `stamp_spans_into`
    // does).
    let nodes = storage.all_code_nodes()?;
    let mut roots: BTreeMap<String, String> = BTreeMap::new();
    for node in &nodes {
        let Some(root) = node.repo_root.clone() else {
            continue;
        };
        if let Some(filter) = repo_filter {
            if root != filter {
                continue;
            }
        }
        let project = roots.entry(root).or_default();
        if project.is_empty() && !node.project.is_empty() {
            *project = node.project.clone();
        }
    }
    // An explicit `--repo` the graph has never touched is still worth
    // trying directly: extraction reads straight from the commit's blob
    // content, not from `code_nodes`, so the graph's root set is a
    // convenience default here, not a correctness gate. No project tag to
    // infer for it.
    if let Some(filter) = repo_filter {
        roots.entry(filter.to_string()).or_default();
    }

    for (repo_root, project) in roots {
        let auditor = match codewitness::Auditor::discover(&repo_root) {
            Ok(a) => a,
            Err(_) => {
                stats.skipped_non_git += 1;
                continue;
            }
        };
        let commit = match auditor.resolve_commit(rev) {
            Ok(c) => c,
            Err(e) => {
                stats.skipped_rev_unresolved += 1;
                eprintln!(
                    "CSR stamp-spans --at {rev}: does not resolve in {repo_root} (skip) ({e})"
                );
                continue;
            }
        };
        let at_oid_str = commit.to_string();
        stats
            .at_commits
            .push((repo_root.clone(), at_oid_str.clone()));

        let relpaths = match auditor.files_at(commit) {
            Ok(v) => v,
            Err(e) => {
                stats.skipped_non_git += 1;
                eprintln!(
                    "CSR stamp-spans --at {rev}: failed to walk tree of {at_oid_str} in {repo_root} (skip) ({e})"
                );
                continue;
            }
        };

        for relpath in relpaths {
            let relpath_str = relpath.to_string_lossy().to_string();
            if lang_from_path_str(&relpath_str).is_none() {
                continue;
            }
            stats.files_checked += 1;

            let bytes = match auditor.file_content_at(&relpath, commit) {
                Ok(b) => b,
                Err(e) => {
                    stats.skipped_stamp_error += 1;
                    eprintln!(
                        "CSR stamp-spans --at {rev}: blob read error for {repo_root}/{relpath_str} ({e})"
                    );
                    continue;
                }
            };
            let source = match String::from_utf8(bytes) {
                Ok(s) => s,
                Err(_) => {
                    stats.skipped_non_utf8 += 1;
                    continue;
                }
            };
            stats.files_processed += 1;

            // Absolute path, matching the `repo_root`-joined convention
            // every other `file` column in this module uses — keeps a
            // historical row joinable with a future HEAD-mode row for the
            // same file/symbol via `witness_ledger`'s `(project, file,
            // symbol)` key.
            let file_abs = Path::new(&repo_root)
                .join(&relpath)
                .to_string_lossy()
                .to_string();

            // `repo`/`project`/`conv_id`/`session_id` only affect
            // `NodeRow::id` (never consulted here) and `.repo`/`.project`
            // (ditto) — never `.name`/`.kind`/`.span_start`/`.span_end`,
            // which is all this loop reads. Passing the resolved commit as
            // the conv/session placeholders keeps them non-empty without
            // implying any real conversation provenance.
            let fragment = extract_graph_fragment_for_file(
                &source,
                &file_abs,
                &repo_root,
                &project,
                &at_oid_str,
                &at_oid_str,
            );

            // Type-qualified symbol identity (see `qualify_witness_symbols`'s
            // doc comment) — SAME algorithm as `stamp_spans_into`, applied
            // here to the historical blob `source` already read above
            // (never a second, separate re-read/re-extraction) and the
            // `lang` already confirmed `Some` by the `continue` above, so a
            // row minted `--at <rev>` and a row minted at HEAD stay joinable
            // by `(file, symbol)` for the SAME method.
            let lang = lang_from_path_str(&relpath_str)
                .expect("checked Some above via the lang_from_path_str continue-guard");
            let containers = container_spans(&source, lang);
            let defs: Vec<(String, String, i64, i64)> = fragment
                .nodes
                .iter()
                .map(|n| (n.kind.clone(), n.name.clone(), n.span_start, n.span_end))
                .collect();
            let (qualified_symbols, disambiguated) = qualify_witness_symbols(&defs, &containers);
            stats.disambiguated_symbols += disambiguated;

            let mut any_span = false;
            for (idx, node) in fragment.nodes.iter().enumerate() {
                // Synthetic module sentinel — not a real span-level symbol,
                // same exclusion as `stamp_spans_into`.
                if node.kind == "module" {
                    continue;
                }
                if node.span_start < 0 || node.span_end < node.span_start {
                    continue;
                }

                let checked_line = |span: i64| u32::try_from(span.checked_add(1)?).ok();
                let (Some(start_line), Some(end_line)) =
                    (checked_line(node.span_start), checked_line(node.span_end))
                else {
                    stats.skipped_span_out_of_range += 1;
                    eprintln!(
                        "CSR stamp-spans --at {rev}: span {}..{} for {file_abs}::{} does not fit a 1-based u32 line (skip)",
                        node.span_start, node.span_end, node.name
                    );
                    continue;
                };
                any_span = true;

                let symbol = &qualified_symbols[idx];
                let anchor = codewitness::Anchor::new(relpath.clone())
                    .with_symbol(symbol.clone())
                    .with_span(start_line, end_line);

                match auditor.stamp_at(&anchor, commit) {
                    Ok(witness) => {
                        stats.spans_stamped += 1;
                        if dry_run {
                            continue;
                        }
                        let row = WitnessLedgerRow {
                            id: 0,
                            project: project.clone(),
                            file: file_abs.clone(),
                            symbol: Some(symbol.clone()),
                            span_start: Some(node.span_start),
                            span_end: Some(node.span_end),
                            stamp: witness.stamp().as_str().to_string(),
                            tier: "committed".to_string(),
                            at_oid: Some(at_oid_str.clone()),
                            source_kind: "backfill".to_string(),
                            source_id: Some(at_oid_str.clone()),
                        };
                        if let Err(e) = storage.insert_witness(&row) {
                            eprintln!(
                                "CSR stamp-spans --at {rev}: ledger insert error for {file_abs}::{} ({e})",
                                node.name
                            );
                        }
                    }
                    Err(e) => {
                        stats.skipped_stamp_error += 1;
                        eprintln!(
                            "CSR stamp-spans --at {rev}: stamp error for {file_abs}::{} ({e})",
                            node.name
                        );
                    }
                }
            }

            if any_span {
                continue;
            }

            // No function/type/const span in this file at this commit —
            // whole-file fallback witness (symbol = NULL).
            let anchor = codewitness::Anchor::new(relpath.clone());
            match auditor.stamp_at(&anchor, commit) {
                Ok(witness) => {
                    stats.whole_files_stamped += 1;
                    if dry_run {
                        continue;
                    }
                    let row = WitnessLedgerRow {
                        id: 0,
                        project: project.clone(),
                        file: file_abs.clone(),
                        symbol: None,
                        span_start: None,
                        span_end: None,
                        stamp: witness.stamp().as_str().to_string(),
                        tier: "committed".to_string(),
                        at_oid: Some(at_oid_str.clone()),
                        source_kind: "backfill".to_string(),
                        source_id: Some(at_oid_str.clone()),
                    };
                    if let Err(e) = storage.insert_witness(&row) {
                        eprintln!(
                            "CSR stamp-spans --at {rev}: ledger insert error for {file_abs} (whole-file) ({e})"
                        );
                    }
                }
                Err(e) => {
                    stats.skipped_stamp_error += 1;
                    eprintln!(
                        "CSR stamp-spans --at {rev}: whole-file stamp error for {file_abs} ({e})"
                    );
                }
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
        let git = |args: &[&str]| git_in(repo).args(args).status();
        if git(&["init", "-q"]).map(|s| !s.success()).unwrap_or(true) {
            return; // git unavailable — fail-soft skip, matches repo_root.rs precedent
        }
        git(&["config", "user.email", "t@example.com"]).unwrap();
        git(&["config", "user.name", "Test"]).unwrap();

        let file = repo.join("lib.rs");
        std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
        git(&["add", "lib.rs"]).unwrap();
        git(&["commit", "-q", "-m", "introduce foo"]).unwrap();
        let oldest_hash = git_head(repo);

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

    // ─── stamp-spans backfill (v10 "dreaming" substrate) ───

    /// A `git -C <repo>` command with the hook-time environment stripped.
    /// When this suite runs under a `git commit` hook (our pre-commit runs
    /// `cargo test --lib`), git exports `GIT_DIR`/`GIT_INDEX_FILE`/... —
    /// absolute paths in a linked worktree — which would silently redirect
    /// these temp-repo commands at the REAL repository (staging test files
    /// into the actual index). Strip every `GIT_*` var so the command only
    /// ever sees the temp repo.
    fn git_in(repo: &Path) -> Command {
        let mut cmd = Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(&k);
            }
        }
        cmd.arg("-C").arg(repo);
        cmd
    }

    fn init_git_repo(repo: &Path) -> bool {
        let git = |args: &[&str]| git_in(repo).args(args).status();
        if git(&["init", "-q"]).map(|s| !s.success()).unwrap_or(true) {
            return false; // git unavailable — fail-soft skip, matches repo_root.rs precedent
        }
        git(&["config", "user.email", "t@example.com"]).unwrap();
        git(&["config", "user.name", "Test"]).unwrap();
        true
    }

    fn git_head(repo: &Path) -> String {
        String::from_utf8(
            git_in(repo)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string()
    }

    /// Fixture source: two `impl` blocks, each defining a same-named
    /// `is_empty` method — the adversarial-gate audit's proven false
    /// positive (`CodeContext::is_empty` joined to `AstDiff::is_empty` under
    /// the old bare-name identity). 0-based line numbers (matching
    /// `NodeRow::span_start`/`span_end`'s convention): the first
    /// `is_empty` spans lines 3-5 inside `impl CodeContext` (lines 2-6); the
    /// second spans lines 11-13 inside `impl AstDiff` (lines 10-14).
    fn dup_impl_fixture_src() -> &'static str {
        "struct CodeContext;\n\nimpl CodeContext {\n    fn is_empty(&self) -> bool {\n        true\n    }\n}\n\nstruct AstDiff;\n\nimpl AstDiff {\n    fn is_empty(&self) -> bool {\n        false\n    }\n}\n"
    }

    #[test]
    fn collision_rate_probe_fixture_corpus() {
        use ast_grep_language::SupportLang;

        struct Case {
            category: &'static str,
            lang: SupportLang,
            source: &'static str,
            file: &'static str,
            expected_disambiguated: usize,
        }

        let cases = vec![
            Case {
                category: "ts_nested_functions",
                lang: SupportLang::TypeScript,
                source: "function alpha() {\n  function shared() {}\n}\nfunction beta() {\n  function shared() {}\n}\n",
                file: "/repo/nested.ts",
                expected_disambiguated: 0,
            },
            Case {
                category: "ts_object_literal_methods",
                lang: SupportLang::TypeScript,
                source: "const handlers = {\n  left: {\n    run() {}\n  },\n  right: {\n    run() {}\n  }\n};\n",
                file: "/repo/objects.ts",
                expected_disambiguated: 0,
            },
            Case {
                category: "ts_arrow_const_scopes",
                lang: SupportLang::TypeScript,
                source: "const left = () => {\n  function shared() {}\n};\nconst right = () => {\n  function shared() {}\n};\n",
                file: "/repo/arrows.ts",
                expected_disambiguated: 0,
            },
            Case {
                category: "ts_module_overloads",
                lang: SupportLang::TypeScript,
                source: "function parse(value: string) { return value; }\nfunction parse(value: number) { return value; }\nfunction parse(value: string | number) { return value; }\n",
                file: "/repo/overloads.ts",
                expected_disambiguated: 2,
            },
            Case {
                category: "python_nested_defs",
                lang: SupportLang::Python,
                source: "def outer_a():\n    def shared():\n        pass\n\ndef outer_b():\n    def shared():\n        pass\n",
                file: "/repo/nested.py",
                expected_disambiguated: 0,
            },
            Case {
                category: "python_property_pairs",
                lang: SupportLang::Python,
                source: "class Box:\n    @property\n    def value(self):\n        return self._value\n    @value.setter\n    def value(self, value):\n        self._value = value\n",
                file: "/repo/properties.py",
                expected_disambiguated: 1,
            },
        ];

        let mut total_defs = 0usize;
        let mut total_legacy_disambiguated = 0usize;
        let mut total_disambiguated = 0usize;
        for case in cases {
            let fragment = extract_graph_fragment(
                case.source,
                case.lang,
                case.file,
                "repo",
                "proj",
                "conv",
                "session",
            );
            let defs: Vec<_> = fragment
                .nodes
                .iter()
                .filter(|node| node.kind == "function")
                .map(|node| {
                    (
                        node.kind.clone(),
                        node.name.clone(),
                        node.span_start,
                        node.span_end,
                    )
                })
                .collect();
            let containers = container_spans(case.source, case.lang);
            let legacy_containers: Vec<_> = containers
                .iter()
                .filter(|container| !container.name.contains(container.separator))
                .filter(|container| {
                    case.source
                        .lines()
                        .nth(container.start as usize)
                        .is_some_and(|line| line.trim_start().starts_with("class "))
                })
                .cloned()
                .collect();
            let (_, legacy_disambiguated) = qualify_witness_symbols(&defs, &legacy_containers);
            let (symbols, disambiguated) = qualify_witness_symbols(&defs, &containers);
            eprintln!(
                "collision-probe {}: defs={} before_#N={} after_#N={} symbols={symbols:?}",
                case.category,
                defs.len(),
                legacy_disambiguated,
                disambiguated
            );
            assert_eq!(
                disambiguated, case.expected_disambiguated,
                "category {} symbols={symbols:?} containers={containers:?}",
                case.category
            );
            total_defs += defs.len();
            total_legacy_disambiguated += legacy_disambiguated;
            total_disambiguated += disambiguated;
        }
        eprintln!(
            "collision-probe total: defs={total_defs} before_#N={total_legacy_disambiguated} after_#N={total_disambiguated} rate={:.1}%",
            100.0 * total_disambiguated as f64 / total_defs as f64
        );
        assert_eq!((total_defs, total_legacy_disambiguated), (17, 7));
        assert_eq!((total_defs, total_disambiguated), (17, 3));
    }

    fn qualified_fixture_symbols(
        source: &str,
        lang: ast_grep_language::SupportLang,
        defs: &[(&str, i64, i64)],
    ) -> (Vec<String>, usize) {
        let defs: Vec<_> = defs
            .iter()
            .map(|(name, start, end)| ("function".to_string(), (*name).to_string(), *start, *end))
            .collect();
        qualify_witness_symbols(&defs, &container_spans(source, lang))
    }

    #[test]
    fn qualifies_typescript_nested_functions_by_function_parent() {
        use ast_grep_language::SupportLang;

        let source = "function alpha() {\n  function shared() {}\n}\nfunction beta() {\n  function shared() {}\n}\n";
        let (symbols, disambiguated) = qualified_fixture_symbols(
            source,
            SupportLang::TypeScript,
            &[
                ("alpha", 0, 2),
                ("shared", 1, 1),
                ("beta", 3, 5),
                ("shared", 4, 4),
            ],
        );
        assert_eq!(symbols, ["alpha", "alpha.shared", "beta", "beta.shared"]);
        assert_eq!(disambiguated, 0);
    }

    #[test]
    fn qualifies_typescript_object_methods_by_property_key_path() {
        use ast_grep_language::SupportLang;

        let source = "const handlers = {\n  left: {\n    run() {}\n  },\n  right: {\n    run() {}\n  }\n};\n";
        let (symbols, disambiguated) = qualified_fixture_symbols(
            source,
            SupportLang::TypeScript,
            &[("run", 2, 2), ("run", 5, 5)],
        );
        assert_eq!(symbols, ["handlers.left.run", "handlers.right.run"]);
        assert_eq!(disambiguated, 0);
    }

    #[test]
    fn qualifies_typescript_nested_functions_by_arrow_const_parent() {
        use ast_grep_language::SupportLang;

        let source = "const left = () => {\n  function shared() {}\n};\nconst right = () => {\n  function shared() {}\n};\n";
        let (symbols, disambiguated) = qualified_fixture_symbols(
            source,
            SupportLang::TypeScript,
            &[("shared", 1, 1), ("shared", 4, 4)],
        );
        assert_eq!(symbols, ["left.shared", "right.shared"]);
        assert_eq!(disambiguated, 0);
    }

    #[test]
    fn qualifies_python_nested_defs_by_function_parent() {
        use ast_grep_language::SupportLang;

        let source = "def outer_a():\n    def shared():\n        pass\n\ndef outer_b():\n    def shared():\n        pass\n";
        let (symbols, disambiguated) = qualified_fixture_symbols(
            source,
            SupportLang::Python,
            &[
                ("outer_a", 0, 2),
                ("shared", 1, 2),
                ("outer_b", 4, 6),
                ("shared", 5, 6),
            ],
        );
        assert_eq!(
            symbols,
            ["outer_a", "outer_a.shared", "outer_b", "outer_b.shared"]
        );
        assert_eq!(disambiguated, 0);
    }

    #[test]
    fn qualify_witness_symbols_disambiguates_residual_collisions_by_span_order() {
        // Two defs that land on the identical bare symbol (empty
        // `containers` — qualification cannot resolve them, e.g. two free
        // functions or a container this pass couldn't name) must be
        // disambiguated deterministically by SPAN order, never silently
        // joined — and never by `defs`' own input order (index 0 here is
        // the span-LATER occurrence, deliberately, to prove the sort is by
        // span, not position in the input slice).
        let defs = vec![
            ("function".to_string(), "orphan".to_string(), 10i64, 12i64),
            ("function".to_string(), "orphan".to_string(), 1i64, 3i64),
        ];
        let (qualified, disambiguated) = qualify_witness_symbols(&defs, &[]);
        assert_eq!(disambiguated, 1);
        assert_eq!(
            qualified[1], "orphan",
            "span-earliest occurrence stays bare"
        );
        assert_eq!(
            qualified[0], "orphan#2",
            "span-later occurrence gets the safety-net suffix"
        );
    }

    #[test]
    fn qualify_witness_symbols_leaves_non_function_kinds_bare() {
        // `type`/`const` rows are never container-qualified — only
        // `kind == "function"` is (methods are the only shape that can
        // collide across impl/class bodies).
        let defs = vec![("type".to_string(), "Thing".to_string(), 0i64, 2i64)];
        let containers = vec![ContainerSpan {
            name: "Outer".into(),
            separator: "::",
            start: 0,
            end: 5,
        }];
        let (qualified, disambiguated) = qualify_witness_symbols(&defs, &containers);
        assert_eq!(disambiguated, 0);
        assert_eq!(qualified[0], "Thing");
    }

    #[test]
    fn stamp_spans_into_qualifies_symbol_identity_by_impl_container() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        if !init_git_repo(repo) {
            return;
        }

        let file = repo.join("dup.rs");
        std::fs::write(&file, dup_impl_fixture_src()).unwrap();
        git_in(repo).args(["add", "dup.rs"]).status().unwrap();
        git_in(repo)
            .args(["commit", "-q", "-m", "init"])
            .status()
            .unwrap();

        let file_path = file.to_string_lossy().to_string();
        let repo_root = repo.to_string_lossy().to_string();

        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                // Simulate an existing corpus already damaged by the old
                // `(repo,file,kind,name)` collapse: only ONE bare method row
                // survived. The HEAD pass must re-extract the file and mint
                // both physical definitions into a fresh append-only fork.
                let mut diff_is_empty = attr_node("n-diff", &file_path, "is_empty");
                diff_is_empty.repo_root = Some(repo_root.clone());
                diff_is_empty.span_start = 11;
                diff_is_empty.span_end = 13;
                codegraph_upsert_node(conn, &diff_is_empty).unwrap();
                Ok(())
            })
            .unwrap();

        let stats = stamp_spans_into(&storage, false).unwrap();
        assert_eq!(stats.spans_stamped, 2);
        assert_eq!(
            stats.disambiguated_symbols, 0,
            "container qualification alone resolves this — no residual collision"
        );

        let rows = storage.witnesses_for_file("proj", &file_path).unwrap();
        assert_eq!(rows.len(), 2);
        let symbols: BTreeSet<_> = rows.iter().map(|r| r.symbol.clone()).collect();
        assert_eq!(
            symbols,
            BTreeSet::from([
                Some("CodeContext::is_empty".to_string()),
                Some("AstDiff::is_empty".to_string()),
            ]),
            "the two impl-block methods must resolve to DISTINCT container-qualified \
             symbols, never both landing on bare 'is_empty'"
        );
    }

    #[test]
    fn stamp_failure_publishes_no_partial_rederived_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        if !init_git_repo(repo) {
            return;
        }

        let file = repo.join("dup.rs");
        std::fs::write(&file, dup_impl_fixture_src()).unwrap();
        git_in(repo).args(["add", "dup.rs"]).status().unwrap();
        git_in(repo)
            .args(["commit", "-q", "-m", "init"])
            .status()
            .unwrap();

        let file_path = file.to_string_lossy().to_string();
        let repo_root = repo.to_string_lossy().to_string();
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                let mut collapsed = attr_node("n-shared", &file_path, "is_empty");
                collapsed.repo_root = Some(repo_root.clone());
                collapsed.first_conv_id = "conv-partial".into();
                collapsed.last_conv_id = "conv-partial".into();
                codegraph_upsert_node(conn, &collapsed)?;
                Ok(())
            })
            .unwrap();

        let stats = stamp_spans_into_cancellable_with(&storage, false, None, &|anchor, bytes| {
            if anchor.symbol.as_deref() == Some("AstDiff::is_empty") {
                anyhow::bail!("forced sibling stamp failure");
            }
            Ok(codewitness::Auditor::stamp_file_content(anchor, bytes)?
                .as_str()
                .to_string())
        })
        .unwrap();
        assert_eq!(stats.spans_stamped, 1, "one sibling hashes successfully");
        assert_eq!(
            stats.skipped_stamp_error, 1,
            "one sibling is forced to fail"
        );

        storage
            .with_connection(|conn| {
                let published: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM witness_ledger
                     WHERE project = 'proj' AND file = ?1
                       AND source_kind = 'backfill_rederived_v2'",
                    rusqlite::params![file_path],
                    |row| row.get(0),
                )?;
                let incomplete: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM witness_generations
                     WHERE project = 'proj' AND file = ?1 AND status = 'incomplete'",
                    rusqlite::params![file_path],
                    |row| row.get(0),
                )?;
                assert_eq!(
                    published, 0,
                    "successful sibling must not be published alone"
                );
                assert_eq!(incomplete, 1, "failed run remains observable as incomplete");

                let bound = crate::storage::chunk_binding::witness_verdict_for_chunks(
                    conn,
                    &["conv-partial".to_string()],
                )?;
                assert!(
                    bound.is_empty(),
                    "binding must abstain instead of seeing the successful sibling"
                );
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn stamp_spans_into_rederivation_drops_stale_same_name_span() {
        // Persisted graph rows are candidates, not authority. The second
        // row below claims `orphan` at a span whose source actually defines
        // `orphan_two`; production re-extraction must not infer that it is a
        // second `orphan` merely because an old code_nodes row says so.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        if !init_git_repo(repo) {
            return;
        }

        let file = repo.join("dup2.rs");
        std::fs::write(
            &file,
            "fn orphan() {\n    1\n}\n\nfn orphan_two() {\n    2\n}\n",
        )
        .unwrap();
        git_in(repo).args(["add", "dup2.rs"]).status().unwrap();
        git_in(repo)
            .args(["commit", "-q", "-m", "init"])
            .status()
            .unwrap();

        let file_path = file.to_string_lossy().to_string();
        let repo_root = repo.to_string_lossy().to_string();

        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                let mut a = attr_node("n-a", &file_path, "orphan");
                a.repo_root = Some(repo_root.clone());
                a.span_start = 0;
                a.span_end = 2;
                codegraph_upsert_node(conn, &a).unwrap();

                // Stale/corrupt same-name row at `orphan_two`'s span.
                let mut b = attr_node("n-b", &file_path, "orphan");
                b.repo_root = Some(repo_root.clone());
                b.span_start = 4;
                b.span_end = 6;
                codegraph_upsert_node(conn, &b).unwrap();
                Ok(())
            })
            .unwrap();

        let stats = stamp_spans_into(&storage, false).unwrap();
        assert_eq!(stats.spans_stamped, 1);
        assert_eq!(stats.disambiguated_symbols, 0);

        let rows = storage.witnesses_for_file("proj", &file_path).unwrap();
        let symbols: BTreeSet<_> = rows.iter().map(|r| r.symbol.clone()).collect();
        assert_eq!(symbols, BTreeSet::from([Some("orphan".to_string())]),);
    }

    #[test]
    fn stamp_spans_mints_committed_witnesses_matching_head_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        if !init_git_repo(repo) {
            return;
        }

        let file = repo.join("lib.rs");
        std::fs::write(&file, "fn foo() {\n    1\n}\n\nfn bar() {\n    2\n}\n").unwrap();
        git_in(repo).args(["add", "lib.rs"]).status().unwrap();
        git_in(repo)
            .args(["commit", "-q", "-m", "init"])
            .status()
            .unwrap();
        let head = git_head(repo);

        let file_path = file.to_string_lossy().to_string();
        let repo_root = repo.to_string_lossy().to_string();

        let storage = Storage::open_memory().unwrap();
        storage
            .insert_chunk(
                &crate::import::ConversationChunk {
                    id: "chunk-foo-edit".into(),
                    conversation_id: "conv".into(),
                    project_name: "proj".into(),
                    timestamp: "2026-08-09T00:00:00Z".into(),
                    content: "edited foo".into(),
                    message_count: 1,
                    summary: None,
                    author: crate::provenance::Speaker::Assistant,
                    seq: 0,
                    is_sidechain: false,
                },
                &[0.0; 4],
            )
            .unwrap();
        storage
            .with_connection(|conn| {
                let foo_id =
                    crate::extraction::codegraph::node_id("proj", &file_path, "function", "foo");
                let mut foo = attr_node(&foo_id, &file_path, "foo");
                foo.repo_root = Some(repo_root.clone());
                foo.span_start = 0;
                foo.span_end = 2;
                codegraph_upsert_node(conn, &foo).unwrap();
                crate::storage::codegraph::set_last_chunk_id(conn, &foo_id, "chunk-foo-edit")?;

                let bar_id =
                    crate::extraction::codegraph::node_id("proj", &file_path, "function", "bar");
                let mut bar = attr_node(&bar_id, &file_path, "bar");
                bar.repo_root = Some(repo_root.clone());
                bar.span_start = 4;
                bar.span_end = 6;
                codegraph_upsert_node(conn, &bar).unwrap();
                Ok(())
            })
            .unwrap();

        let stats = stamp_spans_into(&storage, false).unwrap();
        assert_eq!(stats.files_processed, 1);
        assert_eq!(stats.spans_stamped, 2, "foo + bar");
        assert_eq!(
            stats.whole_files_stamped, 0,
            "spans exist, no whole-file fallback"
        );

        let rows = storage.witnesses_for_file("proj", &file_path).unwrap();
        assert_eq!(rows.len(), 2);
        for r in &rows {
            assert_eq!(r.tier, "committed");
            assert_eq!(r.at_oid.as_deref(), Some(head.as_str()));
            assert_eq!(r.source_kind, "backfill_rederived_v2");
        }

        // Stamps must match codewitness's own `stamp_at` output exactly.
        let auditor = codewitness::Auditor::discover(&repo_root).unwrap();
        let head_oid: codewitness::ObjectId = head.parse().unwrap();
        let foo_anchor = codewitness::Anchor::new("lib.rs")
            .with_symbol("foo")
            .with_span(1, 3);
        let expected_foo = auditor.stamp_at(&foo_anchor, head_oid).unwrap();
        let foo_row = rows
            .iter()
            .find(|r| r.symbol.as_deref() == Some("foo"))
            .unwrap();
        assert_eq!(foo_row.stamp, expected_foo.stamp().as_str());
        assert_eq!(foo_row.span_start, Some(0));
        assert_eq!(foo_row.span_end, Some(2));
        storage
            .with_connection(|conn| {
                let bound_chunk: String = conn.query_row(
                    "SELECT chunk_id FROM witness_chunk_bindings WHERE witness_id = ?1",
                    rusqlite::params![foo_row.id],
                    |row| row.get(0),
                )?;
                assert_eq!(bound_chunk, "chunk-foo-edit");
                let bar_bindings: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM witness_chunk_bindings b
                     JOIN witness_ledger w ON w.id = b.witness_id
                     WHERE w.symbol = 'bar'",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(bar_bindings, 0, "unattributed sibling must stay unbound");
                Ok(())
            })
            .unwrap();

        // Idempotency plus cadence short-circuit: rerunning at the same HEAD
        // and extractor version must do no blob parsing or span stamping.
        let stats2 = stamp_spans_into(&storage, false).unwrap();
        assert_eq!(stats2.files_processed, 0);
        assert_eq!(stats2.spans_stamped, 0);
        assert_eq!(stats2.skipped_complete_generation, 1);
        let rows_after = storage.witnesses_for_file("proj", &file_path).unwrap();
        assert_eq!(rows_after.len(), 2, "rerun must not add new rows");
    }

    #[test]
    fn stamp_spans_falls_back_to_whole_file_witness_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        if !init_git_repo(repo) {
            return;
        }

        let file = repo.join("data.rs");
        std::fs::write(&file, "// just a comment, no functions\n").unwrap();
        git_in(repo).args(["add", "data.rs"]).status().unwrap();
        git_in(repo)
            .args(["commit", "-q", "-m", "init"])
            .status()
            .unwrap();
        let head = git_head(repo);

        let file_path = file.to_string_lossy().to_string();
        let repo_root = repo.to_string_lossy().to_string();

        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                // Only the synthetic module sentinel — no function/type/const spans.
                let mut module = attr_node("n-mod", &file_path, &file_path);
                module.kind = "module".into();
                module.repo_root = Some(repo_root.clone());
                module.span_start = 0;
                module.span_end = 0;
                codegraph_upsert_node(conn, &module).unwrap();
                Ok(())
            })
            .unwrap();

        let stats = stamp_spans_into(&storage, false).unwrap();
        assert_eq!(stats.spans_stamped, 0);
        assert_eq!(stats.whole_files_stamped, 1);

        let rows = storage.witnesses_for_file("proj", &file_path).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].symbol, None);
        assert_eq!(rows[0].at_oid.as_deref(), Some(head.as_str()));

        // Idempotent: whole-file rows are NULL-keyed, and the COALESCE-based
        // `idx_witness_ledger_identity` UNIQUE index (+ INSERT OR IGNORE)
        // dedupes them at the DB level — see `storage::witness_ledger`'s
        // module doc.
        let stats2 = stamp_spans_into(&storage, false).unwrap();
        assert_eq!(
            stats2.whole_files_stamped, 0,
            "unchanged generation skipped"
        );
        assert_eq!(stats2.skipped_complete_generation, 1);
        let rows_after = storage.witnesses_for_file("proj", &file_path).unwrap();
        assert_eq!(
            rows_after.len(),
            1,
            "whole-file rerun must not duplicate (identity index)"
        );
    }

    #[test]
    fn stamp_spans_skips_span_that_overflows_u32_line_range() {
        // A corrupt persisted span near i64::MAX is not reproduced by the
        // production extractor, so re-derivation drops it without attempting
        // a lossy conversion. With no matching current anchor, the whole-file
        // fallback still fires.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        if !init_git_repo(repo) {
            return;
        }

        let file = repo.join("lib.rs");
        std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
        git_in(repo).args(["add", "lib.rs"]).status().unwrap();
        git_in(repo)
            .args(["commit", "-q", "-m", "init"])
            .status()
            .unwrap();

        let file_path = file.to_string_lossy().to_string();
        let repo_root = repo.to_string_lossy().to_string();

        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                // `span_end + 1` would overflow i64; also exercises the
                // u32::try_from bound via span_start > u32::MAX.
                let mut corrupt = attr_node("n-corrupt", &file_path, "corrupt");
                corrupt.repo_root = Some(repo_root.clone());
                corrupt.span_start = i64::from(u32::MAX) + 1;
                corrupt.span_end = i64::MAX;
                codegraph_upsert_node(conn, &corrupt).unwrap();
                Ok(())
            })
            .unwrap();

        let stats = stamp_spans_into(&storage, false).expect("must never panic or fail");
        assert_eq!(stats.skipped_span_out_of_range, 0);
        assert_eq!(stats.spans_stamped, 0);
        assert_eq!(
            stats.whole_files_stamped, 1,
            "no valid span survived, so the whole-file fallback applies"
        );
        let rows = storage.witnesses_for_file("proj", &file_path).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].symbol, None, "only the whole-file witness lands");
    }

    #[test]
    fn stamp_spans_skips_missing_file_and_non_git_repo_without_failing() {
        let storage = Storage::open_memory().unwrap();

        // (a) file no longer exists on disk.
        storage
            .with_connection(|conn| {
                let mut ghost = attr_node("n-ghost", "/nonexistent/ghost.rs", "ghost_fn");
                ghost.repo_root = Some("/tmp".into());
                ghost.span_start = 0;
                ghost.span_end = 1;
                codegraph_upsert_node(conn, &ghost).unwrap();
                Ok(())
            })
            .unwrap();

        // (b) file exists, but its repo_root has no discoverable git repository.
        let tmp = tempfile::tempdir().unwrap();
        let non_git_dir = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(&non_git_dir).unwrap();
        let plain_file = non_git_dir.join("plain.rs");
        std::fs::write(&plain_file, "fn plain() {}\n").unwrap();
        let plain_path = plain_file.to_string_lossy().to_string();
        storage
            .with_connection(|conn| {
                let mut plain = attr_node("n-plain", &plain_path, "plain");
                plain.repo_root = Some(non_git_dir.to_string_lossy().to_string());
                plain.span_start = 0;
                plain.span_end = 0;
                codegraph_upsert_node(conn, &plain).unwrap();
                Ok(())
            })
            .unwrap();

        let stats = stamp_spans_into(&storage, false).expect("must never fail the run");
        assert_eq!(stats.skipped_file_missing, 1);
        assert_eq!(stats.skipped_non_git, 1);
        assert_eq!(stats.spans_stamped, 0);
        assert_eq!(stats.whole_files_stamped, 0);
    }

    #[test]
    fn stamp_spans_dry_run_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        if !init_git_repo(repo) {
            return;
        }

        let file = repo.join("lib.rs");
        std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
        git_in(repo).args(["add", "lib.rs"]).status().unwrap();
        git_in(repo)
            .args(["commit", "-q", "-m", "init"])
            .status()
            .unwrap();

        let file_path = file.to_string_lossy().to_string();
        let repo_root = repo.to_string_lossy().to_string();

        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                let mut foo = attr_node("n-foo", &file_path, "foo");
                foo.repo_root = Some(repo_root.clone());
                foo.span_start = 0;
                foo.span_end = 2;
                codegraph_upsert_node(conn, &foo).unwrap();
                Ok(())
            })
            .unwrap();

        let stats = stamp_spans_into(&storage, true).unwrap();
        assert_eq!(stats.spans_stamped, 1, "counting still happens");
        assert!(
            storage
                .witnesses_for_file("proj", &file_path)
                .unwrap()
                .is_empty(),
            "dry-run must not write witness_ledger"
        );
    }

    // ─── historical mode: `stamp-spans --at <rev>` ───

    #[test]
    fn stamp_spans_at_qualifies_symbol_identity_by_impl_container() {
        // Real v9.5 shape: coexisting same-named methods in two impl blocks
        // must both survive the production extractor and reach the ledger.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        if !init_git_repo(repo) {
            return;
        }

        let file = repo.join("dup.rs");
        let src = dup_impl_fixture_src();
        std::fs::write(&file, src).unwrap();
        git_in(repo).args(["add", "dup.rs"]).status().unwrap();
        git_in(repo)
            .args(["commit", "-q", "-m", "init"])
            .status()
            .unwrap();
        let commit = git_head(repo);

        let file_abs = file.to_string_lossy().to_string();
        let repo_root = repo.to_string_lossy().to_string();
        let storage = Storage::open_memory().unwrap();

        let stats =
            stamp_spans_historical_into(&storage, &commit, Some(&repo_root), false).unwrap();
        // Two struct declarations + two same-named methods.
        assert_eq!(stats.spans_stamped, 4);
        assert_eq!(stats.disambiguated_symbols, 0);

        let rows = storage.witnesses_for_file("", &file_abs).unwrap();
        assert_eq!(rows.len(), 4);
        let symbols: BTreeSet<_> = rows.iter().map(|r| r.symbol.clone()).collect();
        assert_eq!(
            symbols,
            BTreeSet::from([
                // The struct decl itself is `kind == "type"` — qualification
                // only ever applies to `kind == "function"` (methods).
                Some("CodeContext".to_string()),
                Some("CodeContext::is_empty".to_string()),
                Some("AstDiff".to_string()),
                Some("AstDiff::is_empty".to_string()),
            ]),
            "both impl-block methods survive extraction and receive distinct qualified names"
        );
    }

    #[test]
    fn stamp_spans_at_cross_commit_join_does_not_collide_across_impl_blocks() {
        // The exact scenario the adversarial-gate audit flagged: two
        // UNRELATED `is_empty` methods, in two DIFFERENT `impl` blocks,
        // whose historical witnesses must never look like "the same
        // symbol's history" just because they share a bare name. Modeled
        // across commits (each commit has only ONE `is_empty` definition,
        // so this never touches the separate, out-of-scope `NodeRow`
        // collision `extract_inner`'s own per-parse node collection has
        // for TWO same-named methods co-existing in a SINGLE parse — see
        // `stamp_spans_at_qualifies_symbol_identity_by_impl_container`'s
        // doc comment): commit1 has `CodeContext::is_empty`; commit2
        // REPLACES it with an unrelated `AstDiff::is_empty`. Before this
        // fix, both would mint the identical bare `is_empty` symbol and a
        // naive `(file, symbol)` lookup across the two commits would read
        // as one method's evolution — nonsensical, since they are two
        // unrelated types' methods.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        if !init_git_repo(repo) {
            return;
        }

        let file = repo.join("dup.rs");
        let src1 = "struct CodeContext;\n\nimpl CodeContext {\n    fn is_empty(&self) -> bool {\n        true\n    }\n}\n";
        std::fs::write(&file, src1).unwrap();
        git_in(repo).args(["add", "dup.rs"]).status().unwrap();
        git_in(repo)
            .args(["commit", "-q", "-m", "c1"])
            .status()
            .unwrap();
        let commit1 = git_head(repo);

        // Commit2 replaces CodeContext's impl entirely with AstDiff's —
        // only one `is_empty` definition exists in the file AT EACH commit.
        let src2 = "struct AstDiff;\n\nimpl AstDiff {\n    fn is_empty(&self) -> bool {\n        false\n    }\n}\n";
        std::fs::write(&file, src2).unwrap();
        git_in(repo).args(["add", "dup.rs"]).status().unwrap();
        git_in(repo)
            .args(["commit", "-q", "-m", "c2"])
            .status()
            .unwrap();
        let commit2 = git_head(repo);

        let repo_root = repo.to_string_lossy().to_string();
        let file_abs = file.to_string_lossy().to_string();
        let storage = Storage::open_memory().unwrap();

        stamp_spans_historical_into(&storage, &commit1, Some(&repo_root), false).unwrap();
        stamp_spans_historical_into(&storage, &commit2, Some(&repo_root), false).unwrap();

        let rows = storage.witnesses_for_file("", &file_abs).unwrap();
        // Each commit's file has 1 struct decl (type) + 1 impl method
        // (function) = 2 rows/commit x 2 commits.
        assert_eq!(
            rows.len(),
            4,
            "struct decl + is_empty witness per commit, correctly qualified by ITS OWN container"
        );

        let ctx_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.symbol.as_deref() == Some("CodeContext::is_empty"))
            .collect();
        let diff_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.symbol.as_deref() == Some("AstDiff::is_empty"))
            .collect();
        assert_eq!(ctx_rows.len(), 1, "commit1's witness");
        assert_eq!(diff_rows.len(), 1, "commit2's witness");
        assert_eq!(ctx_rows[0].at_oid.as_deref(), Some(commit1.as_str()));
        assert_eq!(diff_rows[0].at_oid.as_deref(), Some(commit2.as_str()));
        assert!(
            rows.iter().all(|r| r.symbol.as_deref() != Some("is_empty")),
            "no row should ever mint the collision-prone bare symbol for a method \
             defined inside an impl block — a bare-symbol join here would incoherently \
             treat two unrelated types' methods as one symbol's history"
        );
    }

    #[test]
    fn stamp_spans_at_differs_for_changed_symbol_and_matches_for_unchanged_across_two_commits() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        if !init_git_repo(repo) {
            return;
        }

        let file = repo.join("lib.rs");
        std::fs::write(&file, "fn foo() {\n    1\n}\n\nfn bar() {\n    2\n}\n").unwrap();
        git_in(repo).args(["add", "lib.rs"]).status().unwrap();
        git_in(repo)
            .args(["commit", "-q", "-m", "c1"])
            .status()
            .unwrap();
        let commit1 = git_head(repo);

        // Change ONLY foo's body between commits; bar is byte-identical.
        std::fs::write(&file, "fn foo() {\n    999\n}\n\nfn bar() {\n    2\n}\n").unwrap();
        git_in(repo).args(["add", "lib.rs"]).status().unwrap();
        git_in(repo)
            .args(["commit", "-q", "-m", "c2"])
            .status()
            .unwrap();
        let commit2 = git_head(repo);

        let repo_root = repo.to_string_lossy().to_string();
        let file_abs = file.to_string_lossy().to_string();
        let storage = Storage::open_memory().unwrap();

        // Repo the graph has never seen (no `code_nodes` seeded): `--repo`
        // targets it directly, project tag falls back to "".
        let stats1 =
            stamp_spans_historical_into(&storage, &commit1, Some(&repo_root), false).unwrap();
        assert_eq!(stats1.spans_stamped, 2, "foo + bar at commit1");
        assert_eq!(stats1.skipped_rev_unresolved, 0);
        assert_eq!(
            stats1.at_commits,
            vec![(repo_root.clone(), commit1.clone())]
        );
        assert!(stats1
            .format_text(false)
            .contains(&format!("at_commit: {commit1}")));

        let stats2 =
            stamp_spans_historical_into(&storage, &commit2, Some(&repo_root), false).unwrap();
        assert_eq!(stats2.spans_stamped, 2, "foo + bar at commit2");

        let rows = storage.witnesses_for_file("", &file_abs).unwrap();
        assert_eq!(rows.len(), 4, "foo+bar at each of 2 commits");

        let foo_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.symbol.as_deref() == Some("foo"))
            .collect();
        let bar_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.symbol.as_deref() == Some("bar"))
            .collect();
        assert_eq!(foo_rows.len(), 2, "foo stamped at both commits");
        assert_eq!(bar_rows.len(), 2, "bar stamped at both commits");
        for r in rows.iter() {
            assert_eq!(r.tier, "committed");
            assert_eq!(r.source_kind, "backfill");
        }

        assert_ne!(
            foo_rows[0].stamp, foo_rows[1].stamp,
            "foo's body changed between commits — stamps must differ"
        );
        assert_eq!(
            bar_rows[0].stamp, bar_rows[1].stamp,
            "bar is byte-identical across commits — stamps must match"
        );
        let foo_oids: BTreeSet<_> = foo_rows.iter().map(|r| r.at_oid.clone()).collect();
        assert_eq!(
            foo_oids,
            BTreeSet::from([Some(commit1.clone()), Some(commit2.clone())]),
            "each commit's foo row is pinned to its own at_oid"
        );

        // Idempotency: rerunning `--at commit1` must not add new rows.
        let stats1_again =
            stamp_spans_historical_into(&storage, &commit1, Some(&repo_root), false).unwrap();
        assert_eq!(stats1_again.spans_stamped, 2, "re-attempted, not net-new");
        let rows_after = storage.witnesses_for_file("", &file_abs).unwrap();
        assert_eq!(
            rows_after.len(),
            rows.len(),
            "rerun of commit1 must not duplicate rows"
        );
    }

    #[test]
    fn stamp_spans_at_default_visits_known_repo_roots_and_tags_project_from_code_nodes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        if !init_git_repo(repo) {
            return;
        }

        let file = repo.join("lib.rs");
        std::fs::write(&file, "fn only() {}\n").unwrap();
        git_in(repo).args(["add", "lib.rs"]).status().unwrap();
        git_in(repo)
            .args(["commit", "-q", "-m", "c1"])
            .status()
            .unwrap();
        let commit1 = git_head(repo);

        let repo_root = repo.to_string_lossy().to_string();
        let file_path = file.to_string_lossy().to_string();
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                let mut n = attr_node("n-only", &file_path, "only");
                n.repo_root = Some(repo_root.clone());
                n.span_start = 0;
                n.span_end = 0;
                codegraph_upsert_node(conn, &n).unwrap();
                Ok(())
            })
            .unwrap();

        // No `--repo`: the historical backfill must discover this root the
        // same way `stamp_spans_into` discovers its default footprint — via
        // `code_nodes.repo_root` — and tag rows with that root's project.
        let stats = stamp_spans_historical_into(&storage, &commit1, None, false).unwrap();
        assert_eq!(stats.spans_stamped, 1);
        assert_eq!(stats.at_commits, vec![(repo_root.clone(), commit1.clone())]);

        let rows = storage.witnesses_for_file("proj", &file_path).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].symbol.as_deref(), Some("only"));
        assert_eq!(rows[0].project, "proj");
        assert_eq!(rows[0].at_oid.as_deref(), Some(commit1.as_str()));
    }

    #[test]
    fn stamp_spans_at_skips_repo_where_rev_does_not_resolve() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        if !init_git_repo(repo) {
            return;
        }

        let file = repo.join("lib.rs");
        std::fs::write(&file, "fn only() {}\n").unwrap();
        git_in(repo).args(["add", "lib.rs"]).status().unwrap();
        git_in(repo)
            .args(["commit", "-q", "-m", "c1"])
            .status()
            .unwrap();

        let repo_root = repo.to_string_lossy().to_string();
        let storage = Storage::open_memory().unwrap();

        let bogus_rev = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let stats =
            stamp_spans_historical_into(&storage, bogus_rev, Some(&repo_root), false).unwrap();
        assert_eq!(
            stats.skipped_rev_unresolved, 1,
            "a SHA absent from the object database must be a soft skip, never a hard failure"
        );
        assert_eq!(stats.spans_stamped, 0);
        assert!(stats.at_commits.is_empty());
    }

    #[test]
    fn stamp_spans_at_dry_run_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        if !init_git_repo(repo) {
            return;
        }

        let file = repo.join("lib.rs");
        std::fs::write(&file, "fn foo() {\n    1\n}\n").unwrap();
        git_in(repo).args(["add", "lib.rs"]).status().unwrap();
        git_in(repo)
            .args(["commit", "-q", "-m", "c1"])
            .status()
            .unwrap();
        let commit1 = git_head(repo);

        let repo_root = repo.to_string_lossy().to_string();
        let file_abs = file.to_string_lossy().to_string();
        let storage = Storage::open_memory().unwrap();

        let stats =
            stamp_spans_historical_into(&storage, &commit1, Some(&repo_root), true).unwrap();
        assert_eq!(stats.spans_stamped, 1, "counting still happens in dry-run");
        assert!(
            storage
                .witnesses_for_file("", &file_abs)
                .unwrap()
                .is_empty(),
            "dry-run must not write witness_ledger"
        );
    }
}
