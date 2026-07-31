//! Co-edit ledger backfill (WCR Phase 4a internal-binding gate).
//!
//! `code_evolution` is the session↔file co-edit ledger that feeds the B3
//! `coedit:<weight>` corpus-witness tier in `extraction::resolver` (see
//! `resolver::coedit_weight`, which joins `code_evolution` on
//! `(session_id, file_path)`). Live rows come from the `PostToolUse` hook
//! (`hooks::post_tool_use::track_code_evolution`), which only started
//! existing partway through this project's history — sessions before that
//! hook shipped (or that ran with an older binary) have zero co-edit rows,
//! starving B3 of corpus evidence for those projects.
//!
//! This module replays the *entire* on-disk JSONL conversation history and
//! reconstructs the same rows the live hook would have written, so B3 has
//! full corpus coverage regardless of when a session happened.
//!
//! Column conventions mirror `hooks::post_tool_use::track_code_evolution`
//! exactly:
//! - `session_id` column actually holds the **conversation id** (JSONL
//!   filename stem — see `post_tool_use::conv_id_for`'s doc comment), not a
//!   raw `sessionId` field. The reinstatement graph walk and B3 co-edit join
//!   both key on conversation id.
//! - `project_name` is resolved from the edited file's own path (the
//!   directory immediately after `.../projects/` in the file's parent
//!   chain), matching `hooks::post_tool_use::resolve_project_for_hook`'s
//!   primary path. Live-hook fallbacks (`CLAUDE_PROJECT_DIR` /
//!   `MCP_CLIENT_CWD` env vars) have no historical replay equivalent; the
//!   closest substitute is the JSONL record's own `cwd` field, used only
//!   when the file-path derivation comes up empty.
//! - `file_path` is canonicalized via `extraction::repo_path::canonical_repo_path`
//!   before storage. This is a deliberate *improvement* over the live hook
//!   (which currently stores the raw tool-input path): B3's `coedit_weight`
//!   query (`extraction::resolver::canon_path` /
//!   `extraction::resolver::coedit_weight`) canonicalizes its own query
//!   paths before comparing against `code_evolution.file_path`, so
//!   uncanonicalized rows silently fail to join whenever a worktree-relative
//!   path differs from the canonical one. Backfilled rows canonicalize up
//!   front so the join always lines up.
//! - `functions_added` / `functions_removed` / `types_added` / `types_removed`
//!   / `imports_added` / `imports_removed` are left at the schema default
//!   `'[]'` — this backfill reconstructs co-edit *ledger* signal only (which
//!   sessions touched which files), not AST diffs. `import::backfill`
//!   (`csr-engine codegraph backfill`) handles full graph reconstruction
//!   separately.
//! - `language` uses the same extension → language mapping as the hook
//!   (`hooks::post_tool_use::detect_language`).
//!
//! Tool coverage: `Edit`, `Write`, `MultiEdit` (matches the live hook) plus
//! `NotebookEdit` (not currently tracked live, but explicitly in-scope for
//! this backfill for broader co-edit coverage). `NotebookEdit`'s tool input
//! uses `notebook_path` rather than `file_path`; both keys are checked.
//!
//! Idempotency: `id` is deterministic —
//! `"bf-" + sha256(session_id|file_path|timestamp)[..16 hex chars]` — and
//! insertion is `INSERT OR IGNORE` against the `id` PRIMARY KEY. Re-running
//! this backfill (or running it after the live hook has already written the
//! same event) inserts 0 new rows the second time. Existing rows are never
//! modified — only new ids are ever inserted.
//!
//! Rollback: `DELETE FROM code_evolution WHERE id LIKE 'bf-%';` removes every
//! row this module ever wrote, and nothing else.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::hooks::post_tool_use::detect_language;
use crate::import::{discover_projects, list_jsonl_files};
use crate::storage::Storage;

/// Deterministic-id prefix for every row this backfill writes. Used both to
/// build ids and, via `DELETE FROM code_evolution WHERE id LIKE 'bf-%'`, to
/// roll the whole backfill back cleanly.
pub const BACKFILL_ID_PREFIX: &str = "bf-";

/// Tool names this backfill extracts co-edit events from.
const TRACKED_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// Skip any single JSONL line larger than this (malformed / pathological
/// giant tool_use blobs) rather than paying to parse it.
const MAX_LINE_BYTES: usize = 1024 * 1024; // 1MB

/// One reconstructed co-edit event: a single tool_use call that touched one
/// file, ready to become one `code_evolution` row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CoeditEvent {
    /// Conversation id (JSONL filename stem) — stored in the `session_id`
    /// column, matching the live hook's convention.
    conv_id: String,
    project_name: String,
    file_path: String,
    language: &'static str,
    tool_name: String,
    timestamp: String,
}

/// Per-project counts for a `backfill-coedit` run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectCoeditStats {
    /// `--dry-run` only: rows that don't exist yet and would be inserted.
    pub would_insert: usize,
    /// Real run only: rows newly inserted.
    pub inserted: usize,
    /// Rows that already existed (no-op either way — idempotency signal).
    pub skipped: usize,
}

/// Outcome of a `backfill-coedit` run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoeditBackfillStats {
    /// JSONL conversation files scanned.
    pub files_scanned: usize,
    /// Edit/Write/MultiEdit/NotebookEdit tool_use events found across all files.
    pub events_found: usize,
    /// Per-project breakdown, keyed by resolved `project_name` (possibly "").
    pub per_project: BTreeMap<String, ProjectCoeditStats>,
}

impl CoeditBackfillStats {
    /// Total across all projects, in `(would_insert, inserted, skipped)` order.
    pub fn totals(&self) -> (usize, usize, usize) {
        self.per_project.values().fold((0, 0, 0), |acc, p| {
            (
                acc.0 + p.would_insert,
                acc.1 + p.inserted,
                acc.2 + p.skipped,
            )
        })
    }

    /// Human-readable per-project + totals summary.
    pub fn format_text(&self, dry_run: bool) -> String {
        let mode = if dry_run { " (dry-run, no writes)" } else { "" };
        let mut out = format!(
            "CSR co-edit ledger backfill{mode}\n\
             ───────────────────────────────\n\
             files scanned : {}\n\
             events found  : {}\n\n",
            self.files_scanned, self.events_found,
        );
        let (t_would, t_inserted, t_skipped) = self.totals();
        if dry_run {
            out.push_str(
                "project                                  would_insert  already_present\n",
            );
            out.push_str(
                "-----------------------------------------------------------------------\n",
            );
            for (project, stats) in &self.per_project {
                let label = if project.is_empty() {
                    "(unresolved)"
                } else {
                    project
                };
                out.push_str(&format!(
                    "{:<40} {:>12}  {:>15}\n",
                    label, stats.would_insert, stats.skipped
                ));
            }
            out.push_str(
                "-----------------------------------------------------------------------\n",
            );
            out.push_str(&format!(
                "{:<40} {:>12}  {:>15}\n",
                "TOTAL", t_would, t_skipped
            ));
        } else {
            out.push_str("project                                  inserted      skipped\n");
            out.push_str("-----------------------------------------------------------------\n");
            for (project, stats) in &self.per_project {
                let label = if project.is_empty() {
                    "(unresolved)"
                } else {
                    project
                };
                out.push_str(&format!(
                    "{:<40} {:>12}  {:>11}\n",
                    label, stats.inserted, stats.skipped
                ));
            }
            out.push_str("-----------------------------------------------------------------\n");
            out.push_str(&format!(
                "{:<40} {:>12}  {:>11}\n",
                "TOTAL", t_inserted, t_skipped
            ));
        }
        out
    }
}

/// Deterministic backfill row id: `"bf-" + sha256(session_id|file_path|timestamp)[..16 hex]`.
fn backfill_id(session_id: &str, file_path: &str, timestamp: &str) -> String {
    let digest = Sha256::digest(format!("{session_id}|{file_path}|{timestamp}").as_bytes());
    let mut hex = String::with_capacity(16);
    for b in digest.iter() {
        hex.push_str(&format!("{b:02x}"));
        if hex.len() >= 16 {
            break;
        }
    }
    hex.truncate(16);
    format!("{BACKFILL_ID_PREFIX}{hex}")
}

/// Extract the file path a tool_use `input` object targets. `Edit` / `Write`
/// / `MultiEdit` use `file_path`; `NotebookEdit` uses `notebook_path`.
fn extract_target_path(tool_name: &str, input: &serde_json::Value) -> Option<String> {
    let key = if tool_name == "NotebookEdit" {
        "notebook_path"
    } else {
        "file_path"
    };
    input
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Resolve `project_name` for a historical event the same way the live hook
/// resolves it for a live one: primarily from the edited file's own path
/// (walk up to the directory right after `.../projects/`), falling back to
/// the JSONL record's own `cwd` field when the file path itself doesn't
/// contain a `projects` component (the closest historical substitute for the
/// live hook's `CLAUDE_PROJECT_DIR` / `MCP_CLIENT_CWD` fallbacks, neither of
/// which have a replay equivalent).
fn resolve_project_for_backfill(file_path: &str, record_cwd: Option<&str>) -> String {
    if let Some(parent) = Path::new(file_path).parent() {
        let parent_str = parent.to_string_lossy();
        if let Some(p) = crate::search::cross_project::resolve_project_from_cwd(&parent_str) {
            if !p.is_empty() {
                return p;
            }
        }
    }
    if let Some(cwd) = record_cwd {
        if let Some(p) = crate::search::cross_project::resolve_project_from_cwd(cwd) {
            if !p.is_empty() {
                return p;
            }
        }
    }
    String::new()
}

/// Stream one JSONL conversation file line-by-line and extract every
/// Edit/Write/MultiEdit/NotebookEdit tool_use call as a `CoeditEvent`.
/// Malformed / oversized lines are skipped silently — never fatal.
fn extract_events(path: &Path, conv_id: &str) -> Result<Vec<CoeditEvent>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => continue, // malformed encoding etc — tolerate silently
        };
        if line.trim().is_empty() {
            continue;
        }
        if line.len() > MAX_LINE_BYTES {
            continue;
        }
        let parsed: serde_json::Value = match sonic_rs::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if parsed.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let timestamp = match parsed.get("timestamp").and_then(|t| t.as_str()) {
            Some(t) => t.to_string(),
            None => continue, // no timestamp — can't build a deterministic id
        };
        let record_cwd = parsed.get("cwd").and_then(|c| c.as_str());

        let content = match parsed
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        {
            Some(c) => c,
            None => continue,
        };

        for item in content {
            if item.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                continue;
            }
            let tool_name = match item.get("name").and_then(|n| n.as_str()) {
                Some(n) if TRACKED_TOOLS.contains(&n) => n,
                _ => continue,
            };
            let input = match item.get("input") {
                Some(i) => i,
                None => continue,
            };
            let raw_path = match extract_target_path(tool_name, input) {
                Some(p) => p,
                None => continue,
            };

            let project_name = resolve_project_for_backfill(&raw_path, record_cwd);
            let canonical = crate::extraction::repo_path::canonical_repo_path(Path::new(&raw_path));
            let file_path = canonical.to_string_lossy().to_string();
            let language = detect_language(&file_path);

            events.push(CoeditEvent {
                conv_id: conv_id.to_string(),
                project_name,
                file_path,
                language,
                tool_name: tool_name.to_string(),
                timestamp: timestamp.clone(),
            });
        }
    }

    Ok(events)
}

/// Backfill the `code_evolution` co-edit ledger from every JSONL conversation
/// under `projects_dir`. `dry_run` parses + counts everything but writes
/// nothing (existence checks are still read-only lookups, so dry-run counts
/// correctly exclude rows that already exist).
pub fn backfill_coedit(
    storage: &Storage,
    projects_dir: &Path,
    dry_run: bool,
) -> Result<CoeditBackfillStats> {
    let mut stats = CoeditBackfillStats::default();

    // Enumerate every JSONL file across every project dir. The project name
    // `discover_projects` returns (derived from the JSONL directory name) is
    // NOT what we store — see module doc — we only use it here to walk files.
    let projects = discover_projects(projects_dir)?;
    let mut files: Vec<PathBuf> = Vec::new();
    for (dir, _dir_project_name) in &projects {
        match list_jsonl_files(dir) {
            Ok(mut jsonls) => files.append(&mut jsonls),
            Err(e) => eprintln!("CSR coedit-backfill: cannot list {} ({e})", dir.display()),
        }
    }
    files.sort();

    for path in &files {
        stats.files_scanned += 1;
        let conv_id = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let events = match extract_events(path, &conv_id) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("CSR coedit-backfill: skip {} ({e})", path.display());
                continue;
            }
        };

        for ev in events {
            stats.events_found += 1;
            let entry = stats
                .per_project
                .entry(ev.project_name.clone())
                .or_default();
            let id = backfill_id(&ev.conv_id, &ev.file_path, &ev.timestamp);

            if dry_run {
                match storage.code_evolution_id_exists(&id) {
                    Ok(true) => entry.skipped += 1,
                    Ok(false) => entry.would_insert += 1,
                    Err(e) => eprintln!("CSR coedit-backfill: exists-check error for {id} ({e})"),
                }
                continue;
            }

            // Repo identity (WP2 Stage 1, H8 finding): stable across
            // cwd/session boundaries, unlike `project_name` — never
            // overwrites it.
            let repo_root = crate::extraction::repo_root::repo_root_for_file(&ev.file_path);

            match storage.insert_code_evolution_backfill(
                &id,
                &ev.conv_id,
                &ev.project_name,
                &ev.file_path,
                ev.language,
                &ev.tool_name,
                &ev.timestamp,
                repo_root.as_deref(),
            ) {
                Ok(true) => entry.inserted += 1,
                Ok(false) => entry.skipped += 1,
                Err(e) => eprintln!("CSR coedit-backfill: insert error for {id} ({e})"),
            }
        }
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn synthetic_projects(tmp: &Path, jsonl_lines: &[String]) -> PathBuf {
        let proj = tmp.join("-Users-me-projects-demo");
        std::fs::create_dir_all(&proj).unwrap();
        let mut f = std::fs::File::create(proj.join("conv-abc.jsonl")).unwrap();
        for line in jsonl_lines {
            writeln!(f, "{line}").unwrap();
        }
        tmp.to_path_buf()
    }

    fn edit_line(file_path: &str, timestamp: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "timestamp": timestamp,
            "sessionId": "sess-1",
            "cwd": "/Users/me/projects/demo",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "name": "Edit",
                    "input": {
                        "file_path": file_path,
                        "old_string": "",
                        "new_string": "fn foo() {}\n"
                    }
                }]
            }
        })
        .to_string()
    }

    #[test]
    fn extract_events_parses_fixture_line_into_expected_event() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = "/Users/me/projects/demo/src/lib.rs";
        let line = edit_line(file_path, "2026-01-01T00:00:00.000Z");
        let proj = tmp.path().join("-Users-me-projects-demo");
        std::fs::create_dir_all(&proj).unwrap();
        let jsonl = proj.join("conv-xyz.jsonl");
        let mut f = std::fs::File::create(&jsonl).unwrap();
        writeln!(f, "{line}").unwrap();

        let events = extract_events(&jsonl, "conv-xyz").unwrap();
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!(ev.conv_id, "conv-xyz");
        assert_eq!(ev.tool_name, "Edit");
        assert_eq!(ev.timestamp, "2026-01-01T00:00:00.000Z");
        assert_eq!(ev.language, "rust");
        assert_eq!(ev.project_name, "demo");
        // canonical_repo_path falls back to the input path when nothing on
        // disk exists to canonicalize against (no .git ancestor in tmpdir).
        assert!(ev.file_path.ends_with("src/lib.rs"));
    }

    #[test]
    fn extract_events_skips_oversized_and_malformed_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let huge = "x".repeat(MAX_LINE_BYTES + 10);
        let lines = vec![
            "not valid json {{{".to_string(),
            huge,
            edit_line(
                "/Users/me/projects/demo/src/ok.rs",
                "2026-01-01T00:00:00.000Z",
            ),
        ];
        let proj = tmp.path().join("-Users-me-projects-demo");
        std::fs::create_dir_all(&proj).unwrap();
        let jsonl = proj.join("conv-mixed.jsonl");
        let mut f = std::fs::File::create(&jsonl).unwrap();
        for l in &lines {
            writeln!(f, "{l}").unwrap();
        }

        let events = extract_events(&jsonl, "conv-mixed").unwrap();
        assert_eq!(events.len(), 1, "only the valid line yields an event");
        assert!(events[0].file_path.ends_with("ok.rs"));
    }

    #[test]
    fn backfill_id_is_deterministic() {
        let a = backfill_id("s1", "/a/b.rs", "2026-01-01T00:00:00Z");
        let b = backfill_id("s1", "/a/b.rs", "2026-01-01T00:00:00Z");
        let c = backfill_id("s1", "/a/b.rs", "2026-01-01T00:00:01Z");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with(BACKFILL_ID_PREFIX));
        assert_eq!(a.len(), BACKFILL_ID_PREFIX.len() + 16);
    }

    #[test]
    fn dry_run_counts_would_insert_without_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let projects_dir = synthetic_projects(
            &tmp.path().join("corpus"),
            &[edit_line(
                "/Users/me/projects/demo/src/lib.rs",
                "2026-01-01T00:00:00.000Z",
            )],
        );
        let storage = Storage::open_memory().unwrap();

        let stats = backfill_coedit(&storage, &projects_dir, true).unwrap();
        assert_eq!(stats.files_scanned, 1);
        assert_eq!(stats.events_found, 1);
        let (would, inserted, skipped) = stats.totals();
        assert_eq!(would, 1);
        assert_eq!(inserted, 0);
        assert_eq!(skipped, 0);

        // Nothing was actually written.
        let count: i64 = storage
            .with_connection(|conn| {
                conn.query_row("SELECT COUNT(*) FROM code_evolution", [], |r| r.get(0))
                    .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(count, 0, "dry-run must not write code_evolution rows");
    }

    #[test]
    fn real_run_inserts_then_second_run_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let projects_dir = synthetic_projects(
            &tmp.path().join("corpus"),
            &[
                edit_line(
                    "/Users/me/projects/demo/src/lib.rs",
                    "2026-01-01T00:00:00.000Z",
                ),
                edit_line(
                    "/Users/me/projects/demo/src/main.rs",
                    "2026-01-01T00:01:00.000Z",
                ),
            ],
        );
        let storage = Storage::open_memory().unwrap();

        let first = backfill_coedit(&storage, &projects_dir, false).unwrap();
        assert_eq!(first.events_found, 2);
        let (_, inserted1, skipped1) = first.totals();
        assert_eq!(inserted1, 2);
        assert_eq!(skipped1, 0);

        let count: i64 = storage
            .with_connection(|conn| {
                conn.query_row("SELECT COUNT(*) FROM code_evolution", [], |r| r.get(0))
                    .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(count, 2);

        // Second run: same corpus, same DB — must insert 0 new rows.
        let second = backfill_coedit(&storage, &projects_dir, false).unwrap();
        let (_, inserted2, skipped2) = second.totals();
        assert_eq!(inserted2, 0, "idempotent: second run inserts nothing");
        assert_eq!(skipped2, 2, "both rows already existed");

        let count2: i64 = storage
            .with_connection(|conn| {
                conn.query_row("SELECT COUNT(*) FROM code_evolution", [], |r| r.get(0))
                    .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(count2, 2, "row count unchanged by the idempotent re-run");

        // Dry-run after the real run should now report 0 would-insert too.
        let dry_after = backfill_coedit(&storage, &projects_dir, true).unwrap();
        let (would_after, _, skipped_after) = dry_after.totals();
        assert_eq!(would_after, 0);
        assert_eq!(skipped_after, 2);
    }

    #[test]
    fn notebook_edit_uses_notebook_path_field() {
        let tmp = tempfile::tempdir().unwrap();
        let line = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "cwd": "/Users/me/projects/demo",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "name": "NotebookEdit",
                    "input": {
                        "notebook_path": "/Users/me/projects/demo/nb.ipynb",
                        "new_source": "print('hi')"
                    }
                }]
            }
        })
        .to_string();
        let proj = tmp.path().join("-Users-me-projects-demo");
        std::fs::create_dir_all(&proj).unwrap();
        let jsonl = proj.join("conv-nb.jsonl");
        let mut f = std::fs::File::create(&jsonl).unwrap();
        writeln!(f, "{line}").unwrap();

        let events = extract_events(&jsonl, "conv-nb").unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].file_path.ends_with("nb.ipynb"));
        assert_eq!(events[0].tool_name, "NotebookEdit");
    }

    #[test]
    fn read_tool_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let line = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "name": "Read",
                    "input": {"file_path": "/Users/me/projects/demo/src/lib.rs"}
                }]
            }
        })
        .to_string();
        let proj = tmp.path().join("-Users-me-projects-demo");
        std::fs::create_dir_all(&proj).unwrap();
        let jsonl = proj.join("conv-read.jsonl");
        let mut f = std::fs::File::create(&jsonl).unwrap();
        writeln!(f, "{line}").unwrap();

        let events = extract_events(&jsonl, "conv-read").unwrap();
        assert!(events.is_empty(), "Read must not produce a co-edit event");
    }
}
