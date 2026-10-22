//! Extract one source file into the code graph and store it.
//!
//! Shared by the PostToolUse hook (the edit just happened) and the graph drain
//! (the hook ran out of its write budget and queued the file instead,
//! `codegraph::queue_extraction`). Either way the file is read from disk, parsed
//! and stored by [`codegraph::store_extraction`] in one transaction.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use crate::storage::codegraph::{self, Extraction};

/// Largest file re-parsed into the code graph (256 KB).
pub const MAX_GRAPH_FILE_BYTES: u64 = 256 * 1024;

/// One file to extract.
pub struct Job<'a> {
    pub project: &'a str,
    /// Where to read the file from: the path the tool saw (worktree-local).
    pub read_path: &'a str,
    /// The canonical main-repo path it is stored under, so worktree edits do not
    /// create duplicate nodes.
    pub stored_path: &'a str,
    pub conv_id: &'a str,
    pub session_id: &'a str,
    /// The queue entry this extraction answers, or `None` for a fresh edit.
    pub claim: Option<i64>,
}

/// What came of a [`Job`].
#[derive(Debug, PartialEq, Eq)]
pub enum Extracted {
    /// Nothing stored: unsupported language, too large, unreadable, or the
    /// queue entry was taken by a newer extraction.
    Skipped,
    Stored {
        nodes: usize,
        edges: usize,
    },
}

/// Read, parse and store `job`'s file. A file that is gone is `Skipped`, not an
/// error.
pub fn extract_and_store(conn: &Connection, job: &Job<'_>) -> Result<Extracted> {
    let Some(lang) = super::ast_analysis::lang_from_path_str(job.read_path) else {
        return Ok(Extracted::Skipped);
    };
    if std::fs::metadata(job.read_path).is_ok_and(|m| m.len() > MAX_GRAPH_FILE_BYTES) {
        return Ok(Extracted::Skipped);
    }
    let Ok(source) = std::fs::read_to_string(job.read_path) else {
        return Ok(Extracted::Skipped);
    };

    let fragment = super::codegraph::extract_graph_fragment(
        &source,
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
    let source_chunk_id: Option<String> = conn
        .query_row(
            "SELECT id FROM chunks WHERE conversation_id = ?1
             ORDER BY COALESCE(seq, -1) DESC, rowid DESC LIMIT 1",
            params![job.conv_id],
            |row| row.get(0),
        )
        .optional()?;
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
    let content_hash = super::anchors::hash_normalized(&source);
    let stored = codegraph::store_extraction(
        conn,
        &Extraction {
            project: job.project,
            file: job.stored_path,
            content_hash: &content_hash,
            nodes: &nodes,
            source_chunk_id: source_chunk_id.as_deref(),
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
