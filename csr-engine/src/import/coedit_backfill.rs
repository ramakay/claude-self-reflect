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
//! - `file_path` is canonicalized via `extraction::repo_path::canonical_repo_path_cold`
//!   (the cache-free spelling, so a long replay names a file the same way
//!   every time) before storage. This is a deliberate *improvement* over the live hook
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
//! `Bash`: one event per file the command wrote, read by the parser the live
//! hook uses (`hooks::bash_writes`), against the `cwd` the transcript row
//! carries. The row's `tool_name` is `Bash`. A call whose `tool_result` came
//! back an error is dropped, and so is a file no longer on disk. Same
//! idempotency as the edit rows. [`backfill_coedit_once`] runs the replay once
//! per database for the daemon and setup, once the installed hook records Bash.
//!
//! Idempotency: `id` is deterministic —
//! `"bf-" + sha256(session_id|file_path|timestamp)[..16 hex chars]` — and
//! insertion is `INSERT OR IGNORE` against the `id` PRIMARY KEY. Re-running
//! this backfill inserts 0 new rows the second time. Existing rows are never
//! modified — only new ids are ever inserted.
//!
//! The live hook's rows carry `evo_` ids, so the id cannot tell them from a
//! replayed event. Instead, an event is skipped (and counted as
//! `already_live`) when `code_evolution` already holds a non-`bf-` row for
//! the same (session, file, tool). Those triples are read once per run.
//!
//! Rollback: `DELETE FROM code_evolution WHERE id LIKE 'bf-%';` removes every
//! row this module ever wrote, and nothing else.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::hooks::post_tool_use::detect_language;
use crate::import::{discover_projects, list_conversation_jsonl_files};
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
    /// The `tool_use` block's id, kept for a Bash event so a call whose
    /// `tool_result` came back an error can be dropped.
    tool_use_id: Option<String>,
    /// For an event read from `<sid>/subagents/agent-<id>.jsonl`: the
    /// transcript's file stem. `conv_id` is then the parent `<sid>`, which is
    /// where the live hook files a subagent's edits. The stem goes into the
    /// row id so a parent event and a subagent event with the same file and
    /// timestamp stay two rows.
    agent: Option<String>,
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
    /// Events dropped because the live hook already wrote a row for the same
    /// (session, file, tool). Counted in both dry and real runs.
    pub already_live: usize,
}

/// Meta key set once [`backfill_coedit_once`] has replayed the whole history
/// with the Bash reader. The first backfill (Edit/Write only) never set one, so
/// transcripts replayed before the Bash path existed are replayed again by this
/// pass; rows already present are skipped by id.
///
/// This was `coedit_bash_backfill_v1`. That pass ran as soon as a daemon came
/// up on the new binary, before `hook install --apply` had put `Bash` in the
/// PostToolUse matcher, so the Bash edits made between the two never reached
/// file history and the marker said the job was done. `v2` makes those
/// installs run the pass once more, now that it waits for the hook; the ids
/// are deterministic, so the rerun inserts only what is missing.
pub const BASH_BACKFILL_META_KEY: &str = "coedit_bash_backfill_v2";

/// Outcome of a `backfill-coedit` run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoeditBackfillStats {
    /// JSONL conversation files scanned.
    pub files_scanned: usize,
    /// Edit/Write/MultiEdit/NotebookEdit tool_use events found across all files.
    pub events_found: usize,
    /// Per-project breakdown, keyed by resolved `project_name` (possibly "").
    pub per_project: BTreeMap<String, ProjectCoeditStats>,
    /// The same counts keyed by tool (`Edit`, `Write`, `Bash`, ...).
    pub per_tool: BTreeMap<String, ProjectCoeditStats>,
    /// Row inserts that failed (a locked database, say). A run with any of
    /// these is not a complete replay.
    pub insert_errors: usize,
    /// Subagent transcripts (`<sid>/subagents/agent-*.jsonl`) scanned.
    pub subagent_files: usize,
    /// Events read from subagent transcripts, and what became of them. They
    /// are also counted in the totals above.
    pub subagent_events: usize,
    pub subagent_inserted: usize,
    pub subagent_already_live: usize,
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

    /// Events the live hook had already recorded, across all projects.
    pub fn already_live_total(&self) -> usize {
        self.per_project.values().map(|p| p.already_live).sum()
    }

    /// Human-readable per-project + totals summary.
    pub fn format_text(&self, dry_run: bool) -> String {
        let mode = if dry_run { " (dry-run, no writes)" } else { "" };
        let mut out = format!(
            "CSR co-edit ledger backfill{mode}\n\
             ───────────────────────────────\n\
             files scanned : {}\n\
             events found  : {}\n\
             already live  : {} (the hook wrote a row for the same session, file and tool)\n\n",
            self.files_scanned,
            self.events_found,
            self.already_live_total(),
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

/// Whose rows a transcript's events are, from its place under the project
/// directory: `<sid>.jsonl` is session `<sid>`; `<sid>/subagents/<agent>.jsonl`
/// is a subagent of `<sid>`, which is where the live hook files its edits.
/// Any other nested `.jsonl` is not a conversation transcript.
fn transcript_owner(project_dir: &Path, path: &Path) -> Option<(String, Option<String>)> {
    let rel = path.strip_prefix(project_dir).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let stem = |name: &str| name.strip_suffix(".jsonl").map(str::to_string);
    match parts.as_slice() {
        [name] => stem(name).map(|sid| (sid, None)),
        [sid, dir, name] if dir == "subagents" => {
            stem(name).map(|agent| (sid.clone(), Some(agent)))
        }
        _ => None,
    }
}

/// `/private/tmp/x` and `/tmp/x` name one file on macOS (`/tmp`, `/var` and
/// `/etc` are symlinks into `/private`). Drop the prefix so the two spellings
/// compare equal.
fn fold_private(path: &str) -> String {
    for dir in ["tmp", "var", "etc"] {
        if let Some(rest) = path.strip_prefix("/private/") {
            if rest == dir || rest.starts_with(&format!("{dir}/")) {
                return format!("/{rest}");
            }
        }
    }
    path.to_string()
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
/// which have a replay equivalent). A temp path skips the file step, as in
/// the live hook: a scratch file takes its session's project, not its
/// folder name.
fn resolve_project_for_backfill(file_path: &str, record_cwd: Option<&str>) -> String {
    if let Some(parent) = Path::new(file_path)
        .parent()
        .filter(|p| !crate::codemap::identity::is_temp_path(p))
    {
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

/// The events a Bash `tool_use` makes: one per existing file its command wrote
/// (`hooks::bash_writes`, the parser the live hook uses), resolved against the
/// cwd the transcript row carried. Bounded like the live hook: a file outside
/// any repository, or a credential / vendored path, is not recorded. The file
/// must still be on disk — the parser reads literal paths out of the command
/// text, and a file that is gone cannot be told from a path that was never
/// written.
fn bash_events(
    input: &serde_json::Value,
    tool_use_id: Option<&str>,
    record_cwd: Option<&str>,
    conv_id: &str,
    timestamp: &str,
) -> Vec<CoeditEvent> {
    let Some(command) = input.get("command").and_then(|c| c.as_str()) else {
        return Vec::new();
    };
    let cwd = Path::new(record_cwd.unwrap_or(""));
    let mut events = Vec::new();
    for written in crate::hooks::bash_writes::bash_writes(command, cwd) {
        if !written.is_file() {
            continue;
        }
        let raw_path = written.to_string_lossy().to_string();
        let project_name = resolve_project_for_backfill(&raw_path, record_cwd);
        let canonical = crate::extraction::repo_path::canonical_repo_path_cold(&written);
        let file_path = canonical.to_string_lossy().to_string();
        if crate::hooks::post_tool_use::is_unrecordable_path(&file_path) {
            continue;
        }
        let language = detect_language(&file_path);
        // The live hook's presence-row bound for a file it cannot parse.
        if language.is_empty()
            && crate::extraction::repo_root::repo_root_for_file(&file_path).is_none()
        {
            continue;
        }
        events.push(CoeditEvent {
            conv_id: conv_id.to_string(),
            project_name,
            file_path,
            language,
            tool_name: "Bash".to_string(),
            timestamp: timestamp.to_string(),
            tool_use_id: tool_use_id.map(str::to_string),
            agent: None,
        });
    }
    events
}

/// Stream one JSONL conversation file line-by-line and extract every
/// Edit/Write/MultiEdit/NotebookEdit tool_use call, and every file a Bash
/// tool_use wrote, as a `CoeditEvent`. A Bash call whose `tool_result` is an
/// error wrote nothing the user kept and is dropped. Malformed / oversized
/// lines are skipped silently — never fatal.
fn extract_events(path: &Path, conv_id: &str) -> Result<Vec<CoeditEvent>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    let mut failed_calls: std::collections::HashSet<String> = std::collections::HashSet::new();

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

        if parsed.get("type").and_then(|t| t.as_str()) == Some("user") {
            let results = parsed
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array());
            for item in results.into_iter().flatten() {
                let is_failed = item.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                    && item.get("is_error").and_then(|e| e.as_bool()) == Some(true);
                if let Some(id) = item
                    .get("tool_use_id")
                    .and_then(|i| i.as_str())
                    .filter(|_| is_failed)
                {
                    failed_calls.insert(id.to_string());
                }
            }
            continue;
        }
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
                Some(n) if TRACKED_TOOLS.contains(&n) || n == "Bash" => n,
                _ => continue,
            };
            let input = match item.get("input") {
                Some(i) => i,
                None => continue,
            };
            if tool_name == "Bash" {
                let id = item.get("id").and_then(|i| i.as_str());
                events.extend(bash_events(input, id, record_cwd, conv_id, &timestamp));
                continue;
            }
            let raw_path = match extract_target_path(tool_name, input) {
                Some(p) => p,
                None => continue,
            };

            let project_name = resolve_project_for_backfill(&raw_path, record_cwd);
            let canonical =
                crate::extraction::repo_path::canonical_repo_path_cold(Path::new(&raw_path));
            let file_path = canonical.to_string_lossy().to_string();
            let language = detect_language(&file_path);
            // The replay stores what the live hook would: a file it cannot parse
            // only inside a repository and off its credential and vendored
            // paths, so no memory notes or scratch text.
            if language.is_empty()
                && (crate::hooks::post_tool_use::is_unrecordable_path(&file_path)
                    || crate::extraction::repo_root::repo_root_for_file(&file_path).is_none())
            {
                continue;
            }

            events.push(CoeditEvent {
                conv_id: conv_id.to_string(),
                project_name,
                file_path,
                language,
                tool_name: tool_name.to_string(),
                timestamp: timestamp.clone(),
                tool_use_id: None,
                agent: None,
            });
        }
    }

    // The result row follows its call, so the failures are only known now.
    events.retain(|e| {
        e.tool_use_id
            .as_deref()
            .is_none_or(|id| !failed_calls.contains(id))
    });
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
    // Loaded once. A (session, file, tool) the live hook already recorded is
    // not replayed: the hook's rows carry `evo_` ids, so the `bf-` id alone
    // would let every one of them be inserted a second time. The hook's
    // spelling of a path outside any repository is not fixed (its process
    // asks `canonical_repo_path` more than once, and the first answer is the
    // raw path while the later ones are symlink-resolved), so both sides are
    // compared with the macOS `/private` prefix folded away.
    let live: HashSet<(String, String, String)> = storage
        .code_evolution_live_triples()?
        .into_iter()
        .map(|(session, file, tool)| (session, fold_private(&file), tool))
        .collect();

    // Enumerate every JSONL file across every project dir. The project name
    // `discover_projects` returns (derived from the JSONL directory name) is
    // NOT what we store — see module doc — we only use it here to walk files.
    let projects = discover_projects(projects_dir)?;
    // (transcript, session id the rows belong to, subagent file stem)
    let mut files: Vec<(PathBuf, String, Option<String>)> = Vec::new();
    for (dir, _dir_project_name) in &projects {
        match list_conversation_jsonl_files(dir) {
            Ok(jsonls) => {
                for path in jsonls {
                    if let Some((sid, agent)) = transcript_owner(dir, &path) {
                        files.push((path, sid, agent));
                    }
                }
            }
            Err(e) => eprintln!("CSR coedit-backfill: cannot list {} ({e})", dir.display()),
        }
    }
    files.sort();

    for (path, conv_id, agent) in &files {
        stats.files_scanned += 1;
        stats.subagent_files += usize::from(agent.is_some());

        let mut events = match extract_events(path, conv_id) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("CSR coedit-backfill: skip {} ({e})", path.display());
                continue;
            }
        };
        for ev in &mut events {
            ev.agent = agent.clone();
        }

        for ev in events {
            stats.events_found += 1;
            stats.subagent_events += usize::from(ev.agent.is_some());
            // Top-level ids are unchanged. A subagent event adds its
            // transcript's stem, so it cannot collide with a parent event.
            let id = match &ev.agent {
                Some(stem) => backfill_id(
                    &format!("{}|{stem}", ev.conv_id),
                    &ev.file_path,
                    &ev.timestamp,
                ),
                None => backfill_id(&ev.conv_id, &ev.file_path, &ev.timestamp),
            };
            let mut delta = ProjectCoeditStats::default();

            if live.contains(&(
                ev.conv_id.clone(),
                fold_private(&ev.file_path),
                ev.tool_name.clone(),
            )) {
                delta.already_live = 1;
            } else if dry_run {
                match storage.code_evolution_id_exists(&id) {
                    Ok(true) => delta.skipped = 1,
                    Ok(false) => delta.would_insert = 1,
                    Err(e) => eprintln!("CSR coedit-backfill: exists-check error for {id} ({e})"),
                }
            } else {
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
                    Ok(true) => delta.inserted = 1,
                    Ok(false) => delta.skipped = 1,
                    Err(e) => {
                        stats.insert_errors += 1;
                        eprintln!("CSR coedit-backfill: insert error for {id} ({e})");
                    }
                }
            }

            if ev.agent.is_some() {
                stats.subagent_inserted += delta.inserted;
                stats.subagent_already_live += delta.already_live;
            }
            for entry in [
                stats
                    .per_project
                    .entry(ev.project_name.clone())
                    .or_default(),
                stats.per_tool.entry(ev.tool_name.clone()).or_default(),
            ] {
                entry.would_insert += delta.would_insert;
                entry.inserted += delta.inserted;
                entry.skipped += delta.skipped;
                entry.already_live += delta.already_live;
            }
        }
    }

    Ok(stats)
}

/// What [`backfill_coedit_once`] did on this call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BashReplay {
    /// The marker was already set.
    Done,
    /// The installed hook config does not record Bash yet; nothing ran and the
    /// marker is unset.
    WaitingOnHook,
    /// The pass ran. The marker is set only if no insert failed.
    Ran(CoeditBackfillStats),
}

impl BashReplay {
    /// The stats of a pass that ran.
    pub fn stats(self) -> Option<CoeditBackfillStats> {
        match self {
            BashReplay::Ran(stats) => Some(stats),
            _ => None,
        }
    }
}

/// Replay the whole history once per database: the pass that fills in the
/// files Bash calls wrote before the live hook read them. It runs only once
/// the hook config in `settings_path` records Bash, since a pass that finishes
/// earlier would leave every Bash edit made before the hook is installed
/// missing for good; until then it defers and the next call checks again. The
/// marker is set only after a run with no failed insert, so a locked database
/// is retried on the next call; rows are keyed by a deterministic id, so a
/// retry inserts only what is missing.
pub fn backfill_coedit_once(
    storage: &Storage,
    projects_dir: &Path,
    settings_path: &Path,
) -> Result<BashReplay> {
    if storage.get_meta(BASH_BACKFILL_META_KEY)?.is_some() {
        return Ok(BashReplay::Done);
    }
    if !crate::hooks::install::post_tool_use_records_bash(settings_path) {
        return Ok(BashReplay::WaitingOnHook);
    }
    let stats = backfill_coedit(storage, projects_dir, false)?;
    if stats.insert_errors == 0 {
        storage.set_meta(BASH_BACKFILL_META_KEY, &chrono::Utc::now().to_rfc3339())?;
    }
    Ok(BashReplay::Ran(stats))
}

/// Where the replay stands, for `status`: `done`, `waiting_on_hook` (the hook
/// config does not record Bash yet, so file history lacks Bash edits) or
/// `pending` (the hook is in place and the daemon has not run the pass yet).
pub fn bash_replay_state(storage: &Storage, settings_path: &Path) -> &'static str {
    if storage
        .get_meta(BASH_BACKFILL_META_KEY)
        .ok()
        .flatten()
        .is_some()
    {
        "done"
    } else if crate::hooks::install::post_tool_use_records_bash(settings_path) {
        "pending"
    } else {
        "waiting_on_hook"
    }
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
    fn extract_events_files_a_scratch_edit_under_its_session_project() {
        let tmp = tempfile::tempdir().unwrap();
        let line = edit_line(
            "/private/tmp/claude-501/-Users-me-projects-demo/sess-1/scratchpad/fix.py",
            "2026-01-01T00:00:00.000Z",
        );
        let proj = tmp.path().join("-Users-me-projects-demo");
        std::fs::create_dir_all(&proj).unwrap();
        let jsonl = proj.join("conv-tmp.jsonl");
        let mut f = std::fs::File::create(&jsonl).unwrap();
        writeln!(f, "{line}").unwrap();

        let events = extract_events(&jsonl, "conv-tmp").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].project_name, "demo");
    }

    /// An edit the live hook would not store is not replayed either: a file it
    /// cannot parse outside any repository. The same file kind inside one is.
    #[test]
    fn extract_events_keeps_the_hooks_bound_for_an_unparsed_file() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        let in_repo = repo.path().join("notes.md");
        std::fs::write(&in_repo, "x\n").unwrap();
        let outside = tmp.path().join("memory").join("note.md");
        let lines = [
            edit_line(&outside.to_string_lossy(), "2026-01-01T00:00:00.000Z"),
            edit_line(&in_repo.to_string_lossy(), "2026-01-01T00:00:01.000Z"),
        ];
        let proj = tmp.path().join("-Users-me-projects-demo");
        std::fs::create_dir_all(&proj).unwrap();
        let jsonl = proj.join("conv-md.jsonl");
        let mut f = std::fs::File::create(&jsonl).unwrap();
        for l in &lines {
            writeln!(f, "{l}").unwrap();
        }

        let events = extract_events(&jsonl, "conv-md").unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(events[0].file_path.ends_with("notes.md"));
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
        // A notebook has no parsed language, so like the hook it is kept only
        // inside a repository.
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        let notebook = repo.path().join("nb.ipynb");
        std::fs::write(&notebook, "{}").unwrap();
        let line = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-01-01T00:00:00.000Z",
            "cwd": "/Users/me/projects/demo",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "name": "NotebookEdit",
                    "input": {
                        "notebook_path": notebook.to_string_lossy(),
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

    fn bash_line(command: &str, id: &str, timestamp: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "timestamp": timestamp,
            "cwd": "/Users/me/projects/demo",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "id": id,
                    "name": "Bash",
                    "input": {"command": command, "description": "edit"}
                }]
            }
        })
        .to_string()
    }

    fn result_line(id: &str, is_error: bool) -> String {
        serde_json::json!({
            "type": "user",
            "timestamp": "2026-01-01T00:00:09.000Z",
            "message": {
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": id,
                    "is_error": is_error,
                    "content": "x"
                }]
            }
        })
        .to_string()
    }

    fn write_jsonl(dir: &Path, name: &str, lines: &[String]) -> PathBuf {
        let proj = dir.join("-Users-me-projects-demo");
        std::fs::create_dir_all(&proj).unwrap();
        let jsonl = proj.join(name);
        let mut f = std::fs::File::create(&jsonl).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        jsonl
    }

    /// A temp "repository" with two files a Bash command can name.
    fn repo_with_files() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        let rs = repo.path().join("lib.rs");
        let yml = repo.path().join("ci.yml");
        std::fs::write(&rs, "fn a() {}\n").unwrap();
        std::fs::write(&yml, "name: x\n").unwrap();
        (repo, rs, yml)
    }

    const INSTALLED_MATCHER: &str = "Edit|Write|MultiEdit|NotebookEdit|Bash";

    /// A settings file in `dir` whose one PostToolUse entry runs CSR's hook
    /// under `matcher` (`None`: an entry with no matcher at all).
    fn settings_file(dir: &Path, matcher: Option<&str>) -> PathBuf {
        let mut entry = serde_json::json!({
            "hooks": [{"type": "command", "command": "/usr/local/bin/csr-engine hook post-tool-use"}]
        });
        if let Some(m) = matcher {
            entry["matcher"] = m.into();
        }
        let path = dir.join("settings.json");
        let settings = serde_json::json!({"hooks": {"PostToolUse": [entry]}});
        std::fs::write(&path, settings.to_string()).unwrap();
        path
    }

    fn code_evolution_rows(storage: &Storage) -> i64 {
        storage
            .with_connection(|conn| {
                conn.query_row("SELECT COUNT(*) FROM code_evolution", [], |r| r.get(0))
                    .map_err(Into::into)
            })
            .unwrap()
    }

    /// One Edit row (already stored) and one Bash write (not yet) in a corpus.
    fn corpus_with_edit_and_bash(tmp: &Path) -> (tempfile::TempDir, PathBuf) {
        let (repo, rs, _yml) = repo_with_files();
        let corpus = tmp.join("corpus");
        write_jsonl(
            &corpus,
            "conv-gate.jsonl",
            &[
                edit_line(
                    "/Users/me/projects/demo/src/lib.rs",
                    "2026-01-01T00:00:00.000Z",
                ),
                bash_line(
                    &format!("sed -i '' 's/a/b/' {}", rs.display()),
                    "toolu_g",
                    "2026-01-01T00:01:00.000Z",
                ),
            ],
        );
        (repo, corpus)
    }

    fn seed_edit_row(storage: &Storage) {
        storage
            .insert_code_evolution_backfill(
                &backfill_id(
                    "conv-gate",
                    "/Users/me/projects/demo/src/lib.rs",
                    "2026-01-01T00:00:00.000Z",
                ),
                "conv-gate",
                "demo",
                "/Users/me/projects/demo/src/lib.rs",
                "rust",
                "Edit",
                "2026-01-01T00:00:00.000Z",
                None,
            )
            .unwrap();
    }

    #[test]
    fn the_pass_runs_and_sets_the_marker_when_the_matcher_names_bash() {
        let tmp = tempfile::tempdir().unwrap();
        let (_repo, corpus) = corpus_with_edit_and_bash(tmp.path());
        let settings = settings_file(tmp.path(), Some(INSTALLED_MATCHER));
        let storage = Storage::open_memory().unwrap();
        seed_edit_row(&storage);
        assert_eq!(bash_replay_state(&storage, &settings), "pending");
        let ran = backfill_coedit_once(&storage, &corpus, &settings)
            .unwrap()
            .stats()
            .expect("the hook records Bash, so the pass runs");
        assert_eq!(ran.totals(), (0, 1, 1));
        assert!(BASH_BACKFILL_META_KEY.ends_with("_v2"));
        assert!(storage.get_meta(BASH_BACKFILL_META_KEY).unwrap().is_some());
        assert_eq!(bash_replay_state(&storage, &settings), "done");
    }

    #[test]
    fn the_pass_defers_while_the_matcher_lacks_bash() {
        let tmp = tempfile::tempdir().unwrap();
        let (_repo, corpus) = corpus_with_edit_and_bash(tmp.path());
        let settings = settings_file(tmp.path(), Some("Edit|Write|MultiEdit|NotebookEdit"));
        let storage = Storage::open_memory().unwrap();
        seed_edit_row(&storage);
        assert_eq!(
            backfill_coedit_once(&storage, &corpus, &settings).unwrap(),
            BashReplay::WaitingOnHook
        );
        assert!(storage.get_meta(BASH_BACKFILL_META_KEY).unwrap().is_none());
        assert_eq!(code_evolution_rows(&storage), 1, "nothing was inserted");
        assert_eq!(bash_replay_state(&storage, &settings), "waiting_on_hook");

        // The hook install lands; the next call runs the pass.
        let settings = settings_file(tmp.path(), Some(INSTALLED_MATCHER));
        let ran = backfill_coedit_once(&storage, &corpus, &settings).unwrap();
        assert!(matches!(ran, BashReplay::Ran(_)));
        assert_eq!(code_evolution_rows(&storage), 2);
    }

    #[test]
    fn the_pass_defers_with_no_settings_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (_repo, corpus) = corpus_with_edit_and_bash(tmp.path());
        let settings = tmp.path().join("absent").join("settings.json");
        let storage = Storage::open_memory().unwrap();
        assert_eq!(
            backfill_coedit_once(&storage, &corpus, &settings).unwrap(),
            BashReplay::WaitingOnHook
        );
        assert!(storage.get_meta(BASH_BACKFILL_META_KEY).unwrap().is_none());
        assert_eq!(code_evolution_rows(&storage), 0);
    }

    #[test]
    fn the_pass_runs_when_the_matcher_matches_every_tool() {
        for matcher in [None, Some(""), Some("*")] {
            let tmp = tempfile::tempdir().unwrap();
            let (_repo, corpus) = corpus_with_edit_and_bash(tmp.path());
            let settings = settings_file(tmp.path(), matcher);
            let storage = Storage::open_memory().unwrap();
            let outcome = backfill_coedit_once(&storage, &corpus, &settings).unwrap();
            assert!(matches!(outcome, BashReplay::Ran(_)), "matcher {matcher:?}");
            assert!(storage.get_meta(BASH_BACKFILL_META_KEY).unwrap().is_some());
        }
    }

    /// An install that finished the v1 pass before its hook was live: the v1
    /// marker does not count, v2 runs once and inserts only what is missing.
    #[test]
    fn a_v1_marker_does_not_stop_the_v2_pass_and_it_inserts_only_the_missing_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let (_repo, corpus) = corpus_with_edit_and_bash(tmp.path());
        let settings = settings_file(tmp.path(), Some(INSTALLED_MATCHER));
        let storage = Storage::open_memory().unwrap();
        storage
            .set_meta("coedit_bash_backfill_v1", "2026-10-19T15:52:00Z")
            .unwrap();
        seed_edit_row(&storage);
        assert_eq!(code_evolution_rows(&storage), 1);

        let ran = backfill_coedit_once(&storage, &corpus, &settings)
            .unwrap()
            .stats()
            .expect("v2 is not set, so the pass runs");
        assert_eq!(ran.totals(), (0, 1, 1), "only the Bash row is new");
        assert_eq!(code_evolution_rows(&storage), 2);
        assert_eq!(
            backfill_coedit_once(&storage, &corpus, &settings).unwrap(),
            BashReplay::Done
        );
        assert_eq!(code_evolution_rows(&storage), 2);
    }

    #[test]
    fn a_bash_call_that_wrote_a_file_becomes_a_bash_event_per_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (_repo, rs, yml) = repo_with_files();
        let cmd = format!(
            "sed -i '' 's/a/b/' {} && echo x >> {}",
            rs.display(),
            yml.display()
        );
        let jsonl = write_jsonl(
            tmp.path(),
            "conv-bash.jsonl",
            &[
                bash_line(&cmd, "toolu_1", "2026-01-01T00:00:00.000Z"),
                result_line("toolu_1", false),
            ],
        );

        let events = extract_events(&jsonl, "conv-bash").unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.tool_name == "Bash"));
        assert!(events.iter().all(|e| e.project_name == "demo"));
        assert!(events[0].file_path.ends_with("lib.rs"));
        assert_eq!(events[0].language, "rust");
        assert!(events[1].file_path.ends_with("ci.yml"));
        assert_eq!(events[1].language, "");
    }

    #[test]
    fn a_failed_bash_call_a_missing_file_and_a_non_writing_call_give_no_event() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, rs, _yml) = repo_with_files();
        let gone = repo.path().join("gone.rs");
        let jsonl = write_jsonl(
            tmp.path(),
            "conv-none.jsonl",
            &[
                bash_line(
                    &format!("sed -i '' 's/a/b/' {}", rs.display()),
                    "toolu_fail",
                    "2026-01-01T00:00:00.000Z",
                ),
                result_line("toolu_fail", true),
                bash_line(
                    &format!("echo x > {}", gone.display()),
                    "toolu_gone",
                    "2026-01-01T00:01:00.000Z",
                ),
                bash_line(
                    &format!("cat {}", rs.display()),
                    "toolu_read",
                    "2026-01-01T00:02:00.000Z",
                ),
            ],
        );
        assert!(extract_events(&jsonl, "conv-none").unwrap().is_empty());
    }

    #[test]
    fn a_bash_write_outside_a_repo_or_to_a_secret_is_not_recorded_but_code_is() {
        let tmp = tempfile::tempdir().unwrap();
        let loose = tempfile::tempdir().unwrap();
        let loose_log = loose.path().join("out.log");
        std::fs::write(&loose_log, "x").unwrap();
        let loose_rs = loose.path().join("scratch.rs");
        std::fs::write(&loose_rs, "fn a() {}").unwrap();
        let (repo, _rs, _yml) = repo_with_files();
        let secret = repo.path().join(".env");
        std::fs::write(&secret, "TOKEN=x").unwrap();
        let cmd = format!(
            "echo a > {} ; echo b > {} ; echo c > {} ; echo d > {}",
            loose_log.display(),
            secret.display(),
            loose_rs.display(),
            repo.path().join("lib.rs").display()
        );
        let jsonl = write_jsonl(
            tmp.path(),
            "conv-bound.jsonl",
            &[bash_line(&cmd, "toolu_b", "2026-01-01T00:00:00.000Z")],
        );
        let events = extract_events(&jsonl, "conv-bound").unwrap();
        let files: Vec<&str> = events
            .iter()
            .map(|e| e.file_path.rsplit('/').next().unwrap())
            .collect();
        // Same bound as the live hook: a code file is recorded wherever it is,
        // a file it cannot parse only inside a repository and never a secret.
        assert_eq!(files, ["scratch.rs", "lib.rs"]);
    }

    /// A row the way the live hook writes it: an `evo_` id.
    fn hook_row(storage: &Storage, session: &str, file: &Path, tool: &str) {
        let stored = crate::extraction::repo_path::canonical_repo_path(file);
        assert!(storage
            .insert_code_evolution_backfill(
                &format!("evo_{session}_{tool}_{}", file.display()),
                session,
                "demo",
                &stored.to_string_lossy(),
                "rust",
                tool,
                "2026-01-01T00:00:00.000Z",
                None,
            )
            .unwrap());
    }

    fn tool_counts(storage: &Storage) -> Vec<(String, i64)> {
        storage
            .with_connection(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT tool_name, COUNT(*) FROM code_evolution
                     WHERE id LIKE 'bf-%' GROUP BY 1 ORDER BY 1",
                )?;
                let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
            })
            .unwrap()
    }

    #[test]
    fn a_session_file_and_tool_the_hook_already_recorded_is_not_replayed() {
        let tmp = tempfile::tempdir().unwrap();
        let (_repo, rs, yml) = repo_with_files();
        let corpus = tmp.path().join("corpus");
        write_jsonl(
            &corpus,
            "conv-live.jsonl",
            &[
                edit_line(&rs.to_string_lossy(), "2026-01-01T00:00:01.000Z"),
                edit_line(&rs.to_string_lossy(), "2026-01-01T00:00:02.000Z"),
                bash_line(
                    &format!("sed -i '' 's/a/b/' {}", rs.display()),
                    "toolu_f",
                    "2026-01-01T00:00:03.000Z",
                ),
                bash_line(
                    &format!("echo y >> {}", yml.display()),
                    "toolu_g",
                    "2026-01-01T00:00:04.000Z",
                ),
            ],
        );
        let storage = Storage::open_memory().unwrap();
        hook_row(&storage, "conv-live", &rs, "Edit");

        let dry = backfill_coedit(&storage, &corpus, true).unwrap();
        assert_eq!(dry.events_found, 4);
        assert_eq!(dry.already_live_total(), 2, "both Edits");
        assert_eq!(dry.totals(), (2, 0, 0));

        let first = backfill_coedit(&storage, &corpus, false).unwrap();
        assert_eq!(first.totals(), (0, 2, 0), "the two Bash rows");
        assert_eq!(first.already_live_total(), 2);
        assert_eq!(tool_counts(&storage), vec![("Bash".to_string(), 2)]);

        let second = backfill_coedit(&storage, &corpus, false).unwrap();
        assert_eq!(second.totals(), (0, 0, 2), "a second pass inserts nothing");
        assert_eq!(second.already_live_total(), 2);
        assert_eq!(tool_counts(&storage), vec![("Bash".to_string(), 2)]);
    }

    #[test]
    fn a_hook_bash_row_suppresses_the_replayed_bash_event_for_that_file_only() {
        let tmp = tempfile::tempdir().unwrap();
        let (_repo, rs, yml) = repo_with_files();
        let corpus = tmp.path().join("corpus");
        write_jsonl(
            &corpus,
            "conv-bash.jsonl",
            &[
                bash_line(
                    &format!("sed -i '' 's/a/b/' {}", rs.display()),
                    "toolu_f",
                    "2026-01-01T00:00:03.000Z",
                ),
                bash_line(
                    &format!("echo y >> {}", yml.display()),
                    "toolu_g",
                    "2026-01-01T00:00:04.000Z",
                ),
            ],
        );
        let storage = Storage::open_memory().unwrap();
        hook_row(&storage, "conv-bash", &yml, "Bash");
        // Another session's hook row for the same file does not suppress.
        hook_row(&storage, "conv-other", &rs, "Bash");

        let stats = backfill_coedit(&storage, &corpus, false).unwrap();
        assert_eq!(stats.totals(), (0, 1, 0));
        assert_eq!(stats.already_live_total(), 1);
        let files: Vec<String> = storage
            .with_connection(|conn| {
                let mut stmt =
                    conn.prepare("SELECT file_path FROM code_evolution WHERE id LIKE 'bf-%'")?;
                let rows = stmt.query_map([], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
            })
            .unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("lib.rs"), "{files:?}");
    }

    /// `<project>/<sid>/<sub>/<name>`, the way a subagent transcript sits.
    fn write_nested(dir: &Path, sid: &str, sub: &str, name: &str, lines: &[String]) {
        let nested = dir.join("-Users-me-projects-demo").join(sid).join(sub);
        std::fs::create_dir_all(&nested).unwrap();
        let mut f = std::fs::File::create(nested.join(name)).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn bf_rows(storage: &Storage) -> Vec<(String, String, String)> {
        storage
            .with_connection(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT session_id, tool_name, file_path FROM code_evolution
                     WHERE id LIKE 'bf-%' ORDER BY file_path, id",
                )?;
                let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
            })
            .unwrap()
    }

    #[test]
    fn subagent_transcripts_are_replayed_under_the_parent_session() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, rs, yml) = repo_with_files();
        let other = repo.path().join("notes.md");
        std::fs::write(&other, "x").unwrap();
        let corpus = tmp.path().join("corpus");
        let ts = "2026-01-01T00:00:00.000Z";
        write_jsonl(
            &corpus,
            "sid-1.jsonl",
            &[bash_line(
                &format!("sed -i '' s/a/b/ {}", rs.display()),
                "t1",
                ts,
            )],
        );
        write_nested(
            &corpus,
            "sid-1",
            "subagents",
            "agent-a1.jsonl",
            &[bash_line(&format!("echo y >> {}", yml.display()), "t2", ts)],
        );
        // Not a conversation transcript: ignored.
        write_nested(
            &corpus,
            "sid-1",
            "tool-results",
            "agent-zz.jsonl",
            &[bash_line(
                &format!("echo y >> {}", other.display()),
                "t3",
                ts,
            )],
        );
        let storage = Storage::open_memory().unwrap();

        let first = backfill_coedit(&storage, &corpus, false).unwrap();
        assert_eq!(first.files_scanned, 2);
        assert_eq!(first.subagent_files, 1);
        assert_eq!(first.subagent_events, 1);
        assert_eq!(first.subagent_inserted, 1);
        assert_eq!(first.totals(), (0, 2, 0));
        let rows = bf_rows(&storage);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(
            rows.iter().all(|r| r.0 == "sid-1" && r.1 == "Bash"),
            "{rows:?}"
        );
        assert!(rows.iter().any(|r| r.2.ends_with("ci.yml")));
        assert!(rows.iter().any(|r| r.2.ends_with("lib.rs")));

        let second = backfill_coedit(&storage, &corpus, false).unwrap();
        assert_eq!(second.totals(), (0, 0, 2), "a rerun adds nothing");
    }

    #[test]
    fn a_parent_event_and_a_subagent_event_for_one_file_and_time_are_two_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let (_repo, rs, _yml) = repo_with_files();
        let corpus = tmp.path().join("corpus");
        let ts = "2026-01-01T00:00:00.000Z";
        let line = bash_line(&format!("sed -i '' s/a/b/ {}", rs.display()), "t1", ts);
        write_jsonl(&corpus, "sid-2.jsonl", std::slice::from_ref(&line));
        write_nested(&corpus, "sid-2", "subagents", "agent-b.jsonl", &[line]);
        let storage = Storage::open_memory().unwrap();
        let stats = backfill_coedit(&storage, &corpus, false).unwrap();
        assert_eq!(stats.totals(), (0, 2, 0));
        assert_eq!(bf_rows(&storage).len(), 2);
    }

    #[test]
    fn a_parent_session_hook_row_suppresses_the_subagents_replayed_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let (_repo, rs, yml) = repo_with_files();
        let corpus = tmp.path().join("corpus");
        write_nested(
            &corpus,
            "sid-3",
            "subagents",
            "agent-c.jsonl",
            &[
                edit_line(&rs.to_string_lossy(), "2026-01-01T00:00:01.000Z"),
                edit_line(&yml.to_string_lossy(), "2026-01-01T00:00:02.000Z"),
            ],
        );
        let storage = Storage::open_memory().unwrap();
        // The live hook files a subagent's edit under the parent session.
        hook_row(&storage, "sid-3", &rs, "Edit");
        let stats = backfill_coedit(&storage, &corpus, false).unwrap();
        assert_eq!(stats.subagent_events, 2);
        assert_eq!(stats.subagent_already_live, 1);
        assert_eq!(stats.subagent_inserted, 1);
        let rows = bf_rows(&storage);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(
            rows[0].2.ends_with("ci.yml") && rows[0].0 == "sid-3",
            "{rows:?}"
        );
    }

    #[test]
    fn the_private_prefix_is_folded_for_the_live_match_only() {
        assert_eq!(fold_private("/private/tmp/a/b.py"), "/tmp/a/b.py");
        assert_eq!(fold_private("/private/var/folders/x"), "/var/folders/x");
        assert_eq!(fold_private("/private/etc/hosts"), "/etc/hosts");
        assert_eq!(fold_private("/tmp/a/b.py"), "/tmp/a/b.py");
        assert_eq!(fold_private("/private/other/x"), "/private/other/x");
        assert_eq!(fold_private("/private/tmpfoo/x"), "/private/tmpfoo/x");
        assert_eq!(
            fold_private("/Users/me/private/tmp/x"),
            "/Users/me/private/tmp/x"
        );
    }

    #[test]
    fn a_hook_row_in_the_other_spelling_of_a_symlinked_path_still_matches() {
        let tmp = tempfile::tempdir().unwrap();
        // A file with no `.git` above it, under a possibly symlinked temp dir.
        let loose = tmp.path().join("scratch").join("tool.py");
        std::fs::create_dir_all(loose.parent().unwrap()).unwrap();
        std::fs::write(&loose, "x = 1\n").unwrap();
        let spelled = loose.to_string_lossy().to_string();
        let resolved = std::fs::canonicalize(&loose)
            .unwrap()
            .to_string_lossy()
            .to_string();
        let corpus = tmp.path().join("corpus");
        write_jsonl(
            &corpus,
            "conv-sym.jsonl",
            &[bash_line(
                &format!("echo y >> {spelled}"),
                "toolu_s",
                "2026-01-01T00:00:00.000Z",
            )],
        );
        let storage = Storage::open_memory().unwrap();
        // The hook stored the symlink-resolved spelling.
        assert!(storage
            .insert_code_evolution_backfill(
                "evo_sym",
                "conv-sym",
                "demo",
                &resolved,
                "python",
                "Bash",
                "2026-01-01T00:00:00.000Z",
                None,
            )
            .unwrap());
        let stats = backfill_coedit(&storage, &corpus, false).unwrap();
        // No repository above it: the replay's own bound drops it, or the
        // folded match does. Either way nothing is added twice.
        assert_eq!(stats.totals().1, 0, "{stats:?}");
        assert!(tool_counts(&storage).is_empty());
    }

    #[test]
    fn bash_rows_are_inserted_once_and_a_rerun_adds_none() {
        let tmp = tempfile::tempdir().unwrap();
        let (_repo, rs, _yml) = repo_with_files();
        let corpus = tmp.path().join("corpus");
        write_jsonl(
            &corpus,
            "conv-idem.jsonl",
            &[bash_line(
                &format!("sed -i '' 's/a/b/' {}", rs.display()),
                "toolu_i",
                "2026-01-01T00:00:00.000Z",
            )],
        );
        let storage = Storage::open_memory().unwrap();
        let first = backfill_coedit(&storage, &corpus, false).unwrap();
        assert_eq!(first.totals(), (0, 1, 0));
        let second = backfill_coedit(&storage, &corpus, false).unwrap();
        assert_eq!(second.totals(), (0, 0, 1));
        let tool: String = storage
            .with_connection(|conn| {
                conn.query_row("SELECT tool_name FROM code_evolution", [], |r| r.get(0))
                    .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(tool, "Bash");
    }

    #[test]
    fn the_one_time_pass_runs_once_and_replays_what_an_earlier_pass_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let (_repo, rs, _yml) = repo_with_files();
        let corpus = tmp.path().join("corpus");
        write_jsonl(
            &corpus,
            "conv-once.jsonl",
            &[
                edit_line(
                    "/Users/me/projects/demo/src/lib.rs",
                    "2026-01-01T00:00:00.000Z",
                ),
                bash_line(
                    &format!("sed -i '' 's/a/b/' {}", rs.display()),
                    "toolu_o",
                    "2026-01-01T00:01:00.000Z",
                ),
            ],
        );
        let settings = settings_file(tmp.path(), Some(INSTALLED_MATCHER));
        let storage = Storage::open_memory().unwrap();
        // An earlier release's pass wrote the Edit row only and set no marker.
        storage
            .insert_code_evolution_backfill(
                &backfill_id(
                    "conv-once",
                    "/Users/me/projects/demo/src/lib.rs",
                    "2026-01-01T00:00:00.000Z",
                ),
                "conv-once",
                "demo",
                "/Users/me/projects/demo/src/lib.rs",
                "rust",
                "Edit",
                "2026-01-01T00:00:00.000Z",
                None,
            )
            .unwrap();
        assert!(storage.get_meta(BASH_BACKFILL_META_KEY).unwrap().is_none());

        let first = backfill_coedit_once(&storage, &corpus, &settings)
            .unwrap()
            .stats()
            .unwrap();
        assert_eq!(
            first.totals(),
            (0, 1, 1),
            "the Bash row is new, the Edit row existed"
        );
        assert!(storage.get_meta(BASH_BACKFILL_META_KEY).unwrap().is_some());
        assert!(
            backfill_coedit_once(&storage, &corpus, &settings).unwrap() == BashReplay::Done,
            "the marker makes the second call a lookup"
        );
    }

    #[test]
    fn a_failed_insert_leaves_the_marker_unset_so_the_pass_is_retried() {
        let tmp = tempfile::tempdir().unwrap();
        let (_repo, rs, _yml) = repo_with_files();
        let corpus = tmp.path().join("corpus");
        write_jsonl(
            &corpus,
            "conv-retry.jsonl",
            &[bash_line(
                &format!("sed -i '' 's/a/b/' {}", rs.display()),
                "toolu_r",
                "2026-01-01T00:00:00.000Z",
            )],
        );
        let settings = settings_file(tmp.path(), Some(INSTALLED_MATCHER));
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                conn.execute_batch(
                    "CREATE TRIGGER refuse BEFORE INSERT ON code_evolution
                     BEGIN SELECT RAISE(ABORT, 'locked'); END;",
                )
                .map_err(Into::into)
            })
            .unwrap();
        let failed = backfill_coedit_once(&storage, &corpus, &settings)
            .unwrap()
            .stats()
            .unwrap();
        assert_eq!(failed.insert_errors, 1);
        assert!(storage.get_meta(BASH_BACKFILL_META_KEY).unwrap().is_none());

        storage
            .with_connection(|conn| {
                conn.execute_batch("DROP TRIGGER refuse")
                    .map_err(Into::into)
            })
            .unwrap();
        let retried = backfill_coedit_once(&storage, &corpus, &settings)
            .unwrap()
            .stats()
            .unwrap();
        assert_eq!(retried.totals(), (0, 1, 0));
        assert!(storage.get_meta(BASH_BACKFILL_META_KEY).unwrap().is_some());
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
