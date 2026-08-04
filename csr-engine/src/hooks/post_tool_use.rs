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

    // Repo/project identity is derivable from `file_path` alone (no lang
    // needed), so compute it before the language gate — WP2 Stage 3 (H8
    // innovation, receipt R4) wants an honest file-level row even for
    // extensions the extractor doesn't parse, instead of the file vanishing
    // silently.
    let project = resolve_project_for_hook(Path::new(file_path).parent());
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
    for node in &fragment.nodes {
        let mut node = node.clone();
        node.repo_root = repo_root.clone();
        storage.upsert_code_node(&node)?;
    }
    storage.replace_code_file_edges(&project, &stored_path_str, &fragment.edges)?;

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

    // As of this fix: this column holds conversation ids (transcript stem), not raw session ids —
    // see conv_id_for. Needed so the reinstatement graph walk (which joins on conversation_id)
    // finds code evolution rows for sidechain/resumed sessions too.
    let conv_id = conv_id_for(input);

    // Resolve project name for cross-project scoping
    let project_name = resolve_project_for_hook(Path::new(file_path).parent());

    // Serialize to JSON arrays
    let fa = serde_json::to_string(&diff.functions_added).unwrap_or_default();
    let fr = serde_json::to_string(&diff.functions_removed).unwrap_or_default();
    let ta = serde_json::to_string(&diff.types_added).unwrap_or_default();
    let tr = serde_json::to_string(&diff.types_removed).unwrap_or_default();
    let ia = serde_json::to_string(&diff.imports_added).unwrap_or_default();
    let ir = serde_json::to_string(&diff.imports_removed).unwrap_or_default();

    // Repo identity (WP2 Stage 1, H8 finding): stable across cwd/session
    // boundaries, unlike `project_name` — never overwrites it.
    let repo_root = crate::extraction::repo_root::repo_root_for_file(file_path);

    engine.storage().insert_code_evolution(
        &conv_id,
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
        repo_root.as_deref(),
    )?;

    eprintln!(
        "CSR: tracked code evolution for {} (+{} fns, -{} fns)",
        file_path,
        diff.functions_added.len(),
        diff.functions_removed.len()
    );

    Ok(())
}

/// Resolve project name for hook processes (no `MCP_CLIENT_CWD`).
///
/// Fallback chain:
/// 1. parent directory of the edited file (via `resolve_project_from_cwd`)
/// 2. `CLAUDE_PROJECT_DIR` env var
/// 3. `MCP_CLIENT_CWD` via `resolve_current_project` (MCP tool path)
/// 4. empty string
fn resolve_project_for_hook(file_path_parent: Option<&Path>) -> String {
    resolve_project_for_hook_with(
        file_path_parent,
        std::env::var("CLAUDE_PROJECT_DIR").ok(),
        crate::search::cross_project::resolve_current_project(),
    )
}

/// Env-free core of `resolve_project_for_hook`: fallback values are passed
/// in so tests never mutate process-global env vars (CodeRabbit PR #279 —
/// `remove_var` in one test races other tests reading the same vars).
fn resolve_project_for_hook_with(
    file_path_parent: Option<&Path>,
    claude_project_dir: Option<String>,
    current_project: Option<String>,
) -> String {
    if let Some(parent) = file_path_parent {
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
    fn resolve_project_for_hook_from_file_parent() {
        // Env-free variant — passing None for both fallbacks isolates the
        // parent-directory chain without mutating process-global env vars.
        let parent = Path::new("/Users/x/projects/my-repo/src/main.rs").parent();
        let project = resolve_project_for_hook_with(parent, None, None);
        assert_eq!(project, "my-repo");
        assert!(!project.is_empty());
    }
}
