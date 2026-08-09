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
fn is_unrecordable_path(path: &str) -> bool {
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
    let tool_name = input.tool_name.as_deref().unwrap_or("unknown");

    // Mirrors the code-graph policy at update_code_graph (lines 68–88):
    // record an honest file-level presence row instead of silently dropping.
    if language.is_empty() {
        let conv_id = conv_id_for(input);
        let project_name = resolve_project_for_hook(Path::new(file_path).parent());
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

    // Read from the original path (worktree-local); store under the canonical
    // main-repo path so worktree edits do not create duplicate rows (D5 — mirrors
    // `update_code_graph` and `import::coedit_backfill`, the two sibling write
    // paths that already canonicalize before storing).
    let stored_path = crate::extraction::repo_path::canonical_repo_path(Path::new(file_path));
    let stored_path_str = stored_path.to_string_lossy();

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
}
