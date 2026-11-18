//! Extract one snapshot of a source file into the code graph.
//!
//! A PostToolUse hook does not extract: it snapshots the file it just saw
//! ([`read_for_snapshot`]) into `code_graph_snapshots` and leaves the rest to
//! the graph drain, which is therefore the only writer of `code_nodes` and
//! `code_edges` that hooks cause. The drain calls [`extract_and_store`] for each
//! snapshot, in the order the edits were captured, with the attribution the hook
//! captured with it, so per-edit credit does not depend on when the drain runs.

use anyhow::Result;
use rusqlite::Connection;

use crate::storage::codegraph::{self, Extraction};

/// Largest file re-parsed into the code graph (256 KB).
pub const MAX_GRAPH_FILE_BYTES: u64 = 256 * 1024;

/// One snapshot to extract.
pub struct Job<'a> {
    pub project: &'a str,
    /// The path the tool saw (worktree-local). Decides the language; read from
    /// disk only when `content` is `None`.
    pub read_path: &'a str,
    /// The canonical main-repo path it is stored under, so worktree edits do not
    /// create duplicate nodes.
    pub stored_path: &'a str,
    pub conv_id: &'a str,
    pub session_id: &'a str,
    /// The transcript chunk the hook attributed the edit to.
    pub source_chunk_id: Option<&'a str>,
    /// What the hook read. `None` for a row queued by an older build, which
    /// stored no content: the file is read now, and the chunk is looked up now.
    pub content: Option<&'a str>,
    /// The snapshot this answers (`Snapshot::seq`).
    pub claim: i64,
}

/// What came of a [`Job`].
#[derive(Debug, PartialEq, Eq)]
pub enum Extracted {
    /// Nothing stored: unsupported language, too large, unreadable, or a newer
    /// snapshot of the file was consumed first.
    Skipped,
    Stored {
        nodes: usize,
        edges: usize,
    },
}

/// The file's text as a hook should snapshot it, or `None` when it is not
/// extractable: unreadable, not UTF-8, or over [`MAX_GRAPH_FILE_BYTES`]. The
/// file's size and mtime are read before and after; if they moved during the
/// read the file is read once more, so the text is one version of the file.
pub fn read_for_snapshot(path: &str) -> Option<String> {
    let stat = || {
        std::fs::metadata(path)
            .ok()
            .map(|m| (m.len(), m.modified().ok()))
    };
    for _ in 0..2 {
        let before = stat()?;
        if before.0 > MAX_GRAPH_FILE_BYTES {
            return None;
        }
        let text = std::fs::read_to_string(path).ok()?;
        if stat()? == before {
            return Some(text);
        }
    }
    std::fs::read_to_string(path).ok()
}

/// Parse `job`'s snapshot and store it. A snapshot whose file is gone (legacy
/// row) is `Skipped` after being consumed, not an error.
pub fn extract_and_store(conn: &Connection, job: &Job<'_>) -> Result<Extracted> {
    let Some(lang) = super::ast_analysis::lang_from_path_str(job.read_path) else {
        codegraph::consume_snapshot(conn, job.project, job.stored_path, job.claim)?;
        return Ok(Extracted::Skipped);
    };
    let disk;
    let source: &str = match job.content {
        Some(text) => text,
        None => match read_for_snapshot(job.read_path) {
            Some(text) => {
                disk = text;
                &disk
            }
            None => {
                codegraph::consume_snapshot(conn, job.project, job.stored_path, job.claim)?;
                return Ok(Extracted::Skipped);
            }
        },
    };

    let fragment = super::codegraph::extract_graph_fragment(
        source,
        lang,
        job.stored_path,
        job.project,
        job.project,
        job.conv_id,
        job.session_id,
    );
    // Repo identity (WP2 Stage 1, H8 finding): stable across cwd/session
    // boundaries, unlike `project` — never overwrites it.
    let repo_root = super::repo_root::repo_root_for_file(job.stored_path);
    let looked_up: Option<String>;
    let source_chunk_id: Option<&str> = if job.content.is_some() {
        job.source_chunk_id
    } else {
        // A legacy row carries no chunk: the latest one of the conversation now.
        looked_up = codegraph::latest_chunk_id(conn, job.conv_id)?;
        looked_up.as_deref()
    };
    let nodes: Vec<codegraph::NodeRow> = fragment
        .nodes
        .iter()
        .map(|n| {
            let mut n = n.clone();
            n.repo_root = repo_root.clone();
            n
        })
        .collect();
    // Only a CLEAN parse may drive the destructive half. A partial parse still
    // returns some valid definitions — `extract_graph_fragment` sets
    // `parse_clean = false` while `nodes` stays non-empty — and treating that
    // truncated view as authoritative would retire every symbol it failed to
    // see, taking each one's `code_node_attribution` provenance with it. Files
    // are routinely observed mid-edit, so this is a common path, not a corner
    // case. Upserts are additive and safe unconditionally; only retirement and
    // edge replacement are gated. This is the same signal `eval::codegraph` and
    // `import::backfill` already gate on.
    let content_hash = super::anchors::hash_normalized(source);
    let stored = codegraph::store_extraction(
        conn,
        &Extraction {
            project: job.project,
            file: job.stored_path,
            content_hash: &content_hash,
            nodes: &nodes,
            source_chunk_id,
            edges: fragment.parse_clean.then_some(fragment.edges.as_slice()),
            claim: job.claim,
        },
    )?;
    Ok(if stored {
        Extracted::Stored {
            nodes: fragment.nodes.len(),
            edges: fragment.edges.len(),
        }
    } else {
        Extracted::Skipped
    })
}
