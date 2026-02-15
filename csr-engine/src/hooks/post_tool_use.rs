//! PostToolUse hook — tracks file modifications after Edit/Write/MultiEdit/NotebookEdit.
//!
//! Fires after each successful tool call. When a Ralph session is active and the tool
//! is a file-modifying tool, stores a brief reflection noting the file modification
//! for future "what files were modified?" queries.
//!
//! For non-Ralph sessions or non-edit tools, exits immediately.
//! Always returns Ok(()) — never blocks Claude Code (C-2 fix).
//!
//! ## Design Decision: Why only file edits?
//!
//! We intentionally limit PostToolUse tracking to file-modifying tools (Edit, Write,
//! MultiEdit, NotebookEdit) rather than capturing all tool calls. Reasons:
//!
//! 1. **Import-time capture is comprehensive**: `extract_tool_context()` in the JSONL
//!    import pipeline already extracts ALL tool names + params from tool_use blocks.
//!    This gives us searchable tool context for every conversation at index time.
//!
//! 2. **Avoid duplicate storage**: Expanding to all tools would duplicate what import
//!    already captures, adding ~2ms per tool call with no incremental search value.
//!
//! 3. **Ralph iteration memory**: File edits are tracked specifically because Ralph
//!    loops need to know "what files did I touch this iteration?" for stuck detection
//!    and iteration-level dedup. Other tool calls don't serve this purpose.
//!
//! 4. **Reactive injection covers real-time**: The `prompt_submit` hook provides
//!    real-time context injection when relevant, addressing the "availability gap"
//!    between tool execution and next import without per-tool-call overhead.

use std::path::Path;

use anyhow::Result;

use super::ralph_state::RalphState;
use super::HookInput;
use crate::engine::Engine;
use crate::mcp::tools;

/// File-modifying tool names that we track.
const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Handle the post-tool-use hook.
/// Wrapped in catch-all: ALWAYS returns Ok(()) to never block Claude Code (C-2 fix).
pub async fn handle(
    input: &HookInput,
    ralph: Option<&RalphState>,
    engine: &Engine,
    cwd: &Path,
) -> Result<()> {
    // Fast path: no Ralph session → exit silently (no engine work needed)
    if ralph.is_none() {
        return Ok(());
    }

    if let Err(e) = handle_inner(input, ralph, engine, cwd).await {
        eprintln!("CSR: post-tool-use hook error (non-fatal): {}", e);
    }
    Ok(()) // Always succeed
}

async fn handle_inner(
    input: &HookInput,
    ralph: Option<&RalphState>,
    engine: &Engine,
    _cwd: &Path,
) -> Result<()> {
    let ralph = match ralph {
        Some(r) => r,
        None => return Ok(()),
    };

    // Check if this is a file-modifying tool
    let tool_name = match input.tool_name.as_deref() {
        Some(name) if EDIT_TOOLS.contains(&name) => name,
        _ => return Ok(()),
    };

    // Extract file path from tool_input
    let file_path = input
        .tool_input
        .as_ref()
        .and_then(|v| v.get("file_path"))
        .and_then(|v| v.as_str());

    let file_path = match file_path {
        Some(p) => p,
        None => return Ok(()),
    };

    // Dedup: check if this file was already tracked in this session (H-1 fix: tag-based check)
    let session_tag = format!("session_{}", ralph.session_id);
    if is_file_already_tracked(engine, &session_tag, file_path) {
        return Ok(());
    }

    // Store brief reflection about file modification
    let content = format!(
        "File modified: {} (tool: {}, session: {}, iteration: {})",
        file_path, tool_name, ralph.session_id, ralph.iteration
    );

    let tags = vec![
        "file_edit".to_string(),
        session_tag,
        format!("iteration_{}", ralph.iteration),
    ];

    if let Err(e) = tools::store_reflection(
        engine.storage(),
        engine.embeddings(),
        engine.search(),
        &content,
        &tags,
    )
    .await
    {
        eprintln!("CSR: Failed to store file edit tracking: {}", e);
    }

    Ok(())
}

/// Check if a file path has already been tracked in this session.
fn is_file_already_tracked(engine: &Engine, session_tag: &str, file_path: &str) -> bool {
    if let Ok(reflections) = engine.storage().get_reflections_by_tag(session_tag, 50) {
        for (_id, content, tags, _ts) in &reflections {
            let is_file_edit = tags.iter().any(|t| t == "file_edit");
            if is_file_edit && content.contains(file_path) {
                return true;
            }
        }
    }
    false
}
