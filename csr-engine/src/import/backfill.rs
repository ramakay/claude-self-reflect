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

        for (file_path, fragment_src) in &file_code {
            let lang = match lang_from_path_str(file_path) {
                Some(l) => l,
                None => {
                    stats.skipped_unsupported += 1;
                    continue;
                }
            };

            // Prefer the complete on-disk file (resolves callers↔callees);
            // fall back to the concatenated edit fragment only if it's gone.
            let disk = disk_source(file_path, &mut disk_cache);
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

            // Nodes: upsert as we go (oldest-first preserves first_conv_id).
            for node in &fragment.nodes {
                if !dry_run {
                    if let Err(e) = storage.upsert_code_node(node) {
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
}
