//! PostToolUse hook — imports transcript + tracks code evolution.
//!
//! Fires after each successful tool call. Imports the current transcript
//! so new content becomes searchable. For Edit/Write operations, tracks
//! structural code changes (functions/types/imports added/removed).
//!
//! Always returns Ok(()) — never blocks Claude Code (C-2 fix).

use std::path::Path;

use anyhow::Result;

use super::HookInput;
use crate::engine::Engine;

/// Handle the post-tool-use hook.
/// Always returns Ok(()) to never block Claude Code (C-2 fix).
pub async fn handle(input: &HookInput, engine: &Engine, cwd: &Path) -> Result<()> {
    // Import growing transcript (real-time searchability)
    super::import_current_transcript(input, engine, cwd).await;

    // Track code evolution for Edit/Write/MultiEdit operations (v9)
    if let Some(ref tool_name) = input.tool_name {
        if tool_name == "Edit" || tool_name == "Write" || tool_name == "MultiEdit" {
            if let Err(e) = track_code_evolution(input, engine).await {
                eprintln!("CSR: code evolution tracking error (non-fatal): {}", e);
            }
            // v9.4 liveness path: re-extract the touched file into the code graph
            // so callers/callees/ledger reflect the edit immediately.
            if let Err(e) = update_code_graph(input, engine) {
                eprintln!("CSR: code graph update error (non-fatal): {}", e);
            }
        }
    }

    Ok(())
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
fn update_code_graph(input: &HookInput, engine: &Engine) -> Result<()> {
    let tool_input = input
        .tool_input
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no tool_input"))?;
    let file_path = tool_input
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("no file_path in tool_input"))?;

    let lang = match crate::extraction::ast_analysis::lang_from_path_str(file_path) {
        Some(l) => l,
        None => return Ok(()), // unsupported language — skip
    };

    let meta = std::fs::metadata(file_path);
    if let Ok(m) = &meta {
        if m.len() > MAX_GRAPH_FILE_BYTES {
            return Ok(());
        }
    }
    let source = match std::fs::read_to_string(file_path) {
        Ok(s) => s,
        Err(_) => return Ok(()), // file gone/unreadable — skip silently
    };

    let project = crate::search::cross_project::resolve_current_project().unwrap_or_default();
    let session_id = input.session_id.clone().unwrap_or_default();
    let conv_id = conv_id_for(input);

    let fragment = crate::extraction::codegraph::extract_graph_fragment(
        &source,
        lang,
        file_path,
        &project,
        &project,
        &conv_id,
        &session_id,
    );

    let storage = engine.storage();
    for node in &fragment.nodes {
        storage.upsert_code_node(node)?;
    }
    storage.replace_code_file_edges(&project, file_path, &fragment.edges)?;

    let content_hash = crate::extraction::anchors::hash_normalized(&source);
    storage.upsert_code_file_state(&project, file_path, &content_hash, false)?;

    // Re-resolve + re-rank so the live graph is queryable right after the edit.
    let _ = storage.resolve_code_edges(&project)?;
    let _ = storage.compute_code_rank(&project)?;

    eprintln!(
        "CSR: code graph updated for {} ({} nodes, {} edges)",
        file_path,
        fragment.nodes.len(),
        fragment.edges.len()
    );

    Ok(())
}

/// Detect programming language from file extension.
fn detect_language(file_path: &str) -> &'static str {
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
async fn track_code_evolution(input: &HookInput, engine: &Engine) -> Result<()> {
    let tool_input = input
        .tool_input
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no tool_input"))?;
    let file_path = tool_input
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("no file_path in tool_input"))?;

    let language = detect_language(file_path);
    if language.is_empty() {
        return Ok(()); // Unknown language — skip AST analysis
    }

    let tool_name = input.tool_name.as_deref().unwrap_or("unknown");

    // Build before/after pairs depending on tool type (Codex M-2: handle MultiEdit edits array)
    let edit_pairs: Vec<(String, String)> = if tool_name == "Edit" {
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

    // Only store if something structural changed
    if diff.is_empty() {
        return Ok(());
    }

    let session_id = input.session_id.as_deref().unwrap_or("unknown");

    // Resolve project name for cross-project scoping
    let project_name = crate::search::cross_project::resolve_current_project().unwrap_or_default();

    // Serialize to JSON arrays
    let fa = serde_json::to_string(&diff.functions_added).unwrap_or_default();
    let fr = serde_json::to_string(&diff.functions_removed).unwrap_or_default();
    let ta = serde_json::to_string(&diff.types_added).unwrap_or_default();
    let tr = serde_json::to_string(&diff.types_removed).unwrap_or_default();
    let ia = serde_json::to_string(&diff.imports_added).unwrap_or_default();
    let ir = serde_json::to_string(&diff.imports_removed).unwrap_or_default();

    engine.storage().insert_code_evolution(
        session_id,
        &project_name,
        file_path,
        language,
        tool_name,
        &fa,
        &fr,
        &ta,
        &tr,
        &ia,
        &ir,
    )?;

    eprintln!(
        "CSR: tracked code evolution for {} (+{} fns, -{} fns)",
        file_path,
        diff.functions_added.len(),
        diff.functions_removed.len()
    );

    Ok(())
}
