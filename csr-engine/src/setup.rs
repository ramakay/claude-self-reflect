//! Setup subcommand — one-shot system initialization.
//!
//! `csr-engine setup [--anthropic-key=sk-ant-...]`
//!
//! Performs all setup steps:
//! 1. Creates DB
//! 2. Discovers + imports all conversations
//! 3. Runs heuristic enrichment
//! 4. Registers as MCP server (claude mcp add)
//! 5. Installs hooks
//! 6. Optionally saves Anthropic API key
//! 7. Prints summary
//!
//! After those steps succeed, setup installs the csr-companion plugin. The binary writes it as
//! a local marketplace under `~/.claude-self-reflect/csr-companion/marketplace` and Claude
//! Code's own plugin CLI installs it from there (`install_relay`, also `csr-engine companion
//! plugin install`), so the plugin's version is always the engine's. `--no-companion` skips
//! it. A failure there is reported with the one command that retries it and never fails setup.

use std::io::{BufRead, IsTerminal, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::companion::relay::emit;
use crate::daemon::dream_cadence;
use crate::dream::policy::{self, EffortTier, NightEstimate};
use crate::engine::Engine;
use crate::hooks;
use crate::import;
use crate::storage::Storage;

/// Run the full setup flow.
pub async fn handle(
    db_path: &Path,
    projects_dir: &Path,
    anthropic_key: Option<String>,
    companion: bool,
) -> Result<()> {
    eprintln!("\n=== Claude Self-Reflect Setup ===\n");

    // Step 1: Ensure DB directory exists, open the engine, and load the
    // embedding model. The model loads lazily on first embed everywhere else;
    // here a first-run download belongs in front of the user (with progress),
    // and it has to succeed BEFORE the MCP server and hooks are written into
    // Claude's config, so a failed download never leaves setup half-applied.
    let csr_dir = db_path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(csr_dir)?;
    eprintln!("[1/8] Database directory ready: {}", csr_dir.display());
    let eng = Engine::new(db_path, projects_dir)?;
    eng.embeddings().warm()?;
    eprintln!("  Embedding model ready");

    // Step 2: Register as MCP server
    eprintln!("[2/8] Registering MCP server...");
    // A registration failure must not cost the user hooks and import, but it
    // must not be reported as success either: note it here, finish the rest,
    // and print the details once at the end. The returned error is deliberately
    // one line so the caller's own rendering does not repeat the remedy.
    let mcp_error = register_mcp_server().err();
    if mcp_error.is_some() {
        eprintln!("  MCP registration failed; details at the end of setup.");
    }

    // Step 3: Install hooks
    eprintln!("[3/8] Installing hooks...");
    // Like the MCP step: a failure does not cost the user the import, but it
    // is carried to the summary instead of being dropped after this warning.
    let hook_error = hook_install_step(|| hooks::install::handle(true));

    // Step 4: Import conversations
    eprintln!("[4/8] Importing conversations...");

    // Count JSONL files for progress
    let total_files = count_total_files(projects_dir);
    eprintln!("  Found {} JSONL files to process", total_files);

    let imported = eng.import_conversations(None).await?;
    eprintln!("  Imported {} chunks", imported);

    // Optional vendor corpus: absence is intentionally silent/inert, and
    // CSR_NO_CODEX_IMPORT=1 skips it on purpose (first import can take hours).
    if crate::daemon::codex_import_disabled() {
        eprintln!("  Codex rollout import skipped (CSR_NO_CODEX_IMPORT is set)");
    } else if let Some(codex_root) = dirs::home_dir().map(|home| home.join(".codex/sessions")) {
        if codex_root.exists() {
            let harness_strip_root = codex_root.clone();
            let adapter_engine = eng.clone();
            let stats = tokio::task::spawn_blocking(move || {
                crate::import::codex_rollout::import_changed_rollouts(&adapter_engine, &codex_root)
            })
            .await??;
            eprintln!(
                "  Imported {} Codex rollout chunks from {} changed files",
                stats.chunks_imported, stats.files_imported
            );

            // One-time, meta-guarded backfill for rollouts imported before
            // the chunk-timestamp-collapse fix (see
            // `restamp_codex_chunk_timestamps`'s doc). Runs under this same
            // binary, so it always applies the fixed chunker — cheap on a
            // no-op second run (single meta lookup).
            let restamp_storage = eng.storage().clone();
            match tokio::task::spawn_blocking(move || {
                crate::import::codex_rollout::restamp_codex_chunk_timestamps(&restamp_storage)
            })
            .await
            {
                Ok(Ok(restamp_stats)) if !restamp_stats.already_applied => {
                    eprintln!(
                        "  Restamped {} Codex rollout chunks across {} conversations ({} missing file, {} unmatched)",
                        restamp_stats.chunks_updated,
                        restamp_stats.conversations_updated,
                        restamp_stats.conversations_missing_file,
                        restamp_stats.conversations_unmatched,
                    );
                }
                Ok(Ok(_)) => {} // already_applied — silent, nothing to report
                Ok(Err(e)) => eprintln!("  Warning: Codex timestamp backfill failed: {e}"),
                Err(e) => eprintln!("  Warning: Codex timestamp backfill failed: {e}"),
            }

            // One-time, meta-guarded backfill for rollouts imported before the
            // harness-marker-stripping adapter fix: invalidate their stored
            // mtime so the `import_changed_rollouts` pass just above (this
            // same run doesn't re-run it, so do it explicitly here) picks
            // them up through the fixed adapter. Daemon-owned installs get
            // the stronger, self-resuming `harness_strip_batch` path instead
            // (`daemon::mod`); this covers a one-shot `setup` run that never
            // starts the daemon at all.
            let strip_engine = eng.clone();
            match tokio::task::spawn_blocking(move || {
                crate::import::codex_rollout::invalidate_polluted_codex_rollouts(&strip_engine)
            })
            .await
            {
                Ok(Ok(strip_stats)) if !strip_stats.already_applied => {
                    if strip_stats.conversations_invalidated > 0 {
                        eprintln!(
                            "  Flagged {} harness-polluted Codex rollouts for re-import",
                            strip_stats.conversations_invalidated
                        );
                        let reimport_engine = eng.clone();
                        let reimport_root = harness_strip_root.clone();
                        match tokio::task::spawn_blocking(move || {
                            crate::import::codex_rollout::import_changed_rollouts(
                                &reimport_engine,
                                &reimport_root,
                            )
                        })
                        .await
                        {
                            Ok(Ok(reimport_stats)) => eprintln!(
                                "  Re-imported {} Codex rollout chunks across {} files (harness strip)",
                                reimport_stats.chunks_imported, reimport_stats.files_imported
                            ),
                            Ok(Err(e)) => {
                                eprintln!("  Warning: Codex harness-strip re-import failed: {e}")
                            }
                            Err(e) => {
                                eprintln!("  Warning: Codex harness-strip re-import failed: {e}")
                            }
                        }
                    }
                }
                Ok(Ok(_)) => {} // already_applied — silent, nothing to report
                Ok(Err(e)) => eprintln!("  Warning: Codex harness-strip backfill failed: {e}"),
                Err(e) => eprintln!("  Warning: Codex harness-strip backfill failed: {e}"),
            }

            // One-time, meta-guarded backfill: flip already-imported Codex
            // subagent (`thread_spawn`) rollouts to `is_sidechain = 1` so
            // they don't take their own recent-work slot next to their
            // parent (see `RolloutMetadata::is_subagent`'s doc).
            let sidechain_storage = eng.storage().clone();
            match tokio::task::spawn_blocking(move || {
                crate::import::codex_rollout::backfill_codex_subagent_sidechain(&sidechain_storage)
            })
            .await
            {
                Ok(Ok(sidechain_stats)) if !sidechain_stats.already_applied => {
                    if sidechain_stats.conversations_subagent > 0 {
                        eprintln!(
                            "  Marked {} Codex subagent rollouts as sidechains ({} chunks)",
                            sidechain_stats.conversations_subagent, sidechain_stats.chunks_updated
                        );
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => eprintln!("  Warning: Codex subagent sidechain backfill failed: {e}"),
                Err(e) => eprintln!("  Warning: Codex subagent sidechain backfill failed: {e}"),
            }
        }
    }

    // Memory registry spine: one initial scan so setup doesn't leave the table empty
    // until the daemon's first 30-min tick. Kill switch matches the daemon loop.
    if !crate::daemon::memory_registry_disabled() {
        let storage = eng.storage().clone();
        let projects_root = projects_dir.to_path_buf();
        match tokio::task::spawn_blocking(move || {
            crate::import::memory_registry::scan_memory_dirs(&storage, &projects_root)
        })
        .await
        {
            Ok(Ok(stats)) => {
                eprintln!(
                    "  Memory registry: {} files scanned, {} upserted, {} schema misses",
                    stats.files_seen, stats.upserted, stats.schema_misses
                );
            }
            Ok(Err(e)) => {
                eprintln!("  Warning: memory registry scan failed: {e}");
            }
            Err(e) => {
                eprintln!("  Warning: memory registry scan failed: {e}");
            }
        }
    }

    // File history for the files Bash calls wrote (`sed -i`, heredocs, short
    // scripts), replayed once from the transcripts just imported; the daemon
    // does the same for an install that never reruns setup. Meta-guarded, and
    // gated on the same hook check as the daemon: step 3 just installed the
    // hooks, but if that failed the replay waits rather than mark itself done
    // while the live hook still skips Bash.
    {
        use crate::import::coedit_backfill::BashReplay;
        let storage = eng.storage().clone();
        let projects_root = projects_dir.to_path_buf();
        let settings = crate::hooks::install::default_settings_path().unwrap_or_default();
        match tokio::task::spawn_blocking(move || {
            crate::import::coedit_backfill::backfill_coedit_once(
                &storage,
                &projects_root,
                &settings,
            )
        })
        .await
        {
            Ok(Ok(BashReplay::Ran(stats))) => {
                let (_, inserted, _) = stats.totals();
                eprintln!(
                    "  File history: {} edits recorded from {} transcripts ({} already live)",
                    inserted,
                    stats.files_scanned,
                    stats.already_live_total()
                );
                if !stats.complete() {
                    eprintln!(
                        "  File history: incomplete ({} unread, {} failed inserts); \
                         the daemon runs it again at its next start",
                        stats.read_errors, stats.insert_errors
                    );
                }
            }
            Ok(Ok(BashReplay::WaitingOnHook)) => eprintln!(
                "  File history: waiting for the Bash hook (run: csr-engine hook install --apply)"
            ),
            Ok(Ok(BashReplay::Done)) => {}
            Ok(Err(e)) => eprintln!("  Warning: file-history backfill failed: {e}"),
            Err(e) => eprintln!("  Warning: file-history backfill failed: {e}"),
        }
    }

    // Step 5: Run enrichment
    eprintln!("[5/8] Running heuristic enrichment...");
    let (backfilled, enriched) = eng.backfill_and_enrich().await?;
    if backfilled > 0 {
        eprintln!("  Backfilled {} import_state rows", backfilled);
    }
    eprintln!("  Enriched {} conversations", enriched);

    // Step 6: Save Anthropic API key if provided
    if let Some(key) = &anthropic_key {
        eprintln!("[6/8] Saving Anthropic API key...");
        save_anthropic_key(csr_dir, key)?;
        eprintln!("  Saved to {}", csr_dir.join(".env").display());
    } else {
        eprintln!("[6/8] Skipping AI narratives (no --anthropic-key provided)");
    }

    // Step 7: Dreaming consent (Journal v4 P5, locked decision 15). Presented
    // ON by default, with a per-night token estimate computed from the corpus
    // that was just imported — i.e. from real numbers, shown BEFORE the
    // question is asked.
    dreaming_consent_step(eng.storage())?;

    // Step 8: the background daemon. Last, so it never races the import above
    // for the database. Dreams are on by default and only the daemon runs them.
    eprintln!("[8/8] Starting the background daemon...");
    let daemon_text = {
        let (db, projects) = (db_path.to_path_buf(), projects_dir.to_path_buf());
        tokio::task::spawn_blocking(move || daemon_step_live(&db, &projects))
            .await
            .unwrap_or_else(|e| format!("  The daemon step did not finish: {e}\n"))
    };
    eprint!("{daemon_text}");

    // Summary
    let conversations = eng.storage().count_conversations().unwrap_or(0);
    let reflections = eng.storage().count_reflection_embeddings().unwrap_or(0);
    let projects = eng.storage().count_projects().unwrap_or(0);

    if mcp_error.is_some() || hook_error.is_some() {
        eprintln!("\n=== Setup Incomplete ===\n");
        eprintln!("  Conversations: {}", conversations);
        eprintln!("  Reflections:   {}", reflections);
        eprintln!("  Projects:      {}", projects);
        eprintln!();
        eprint!(
            "{}",
            incomplete_report(mcp_error.as_ref(), hook_error.as_deref())
        );
        eprintln!();
        // Short on purpose: the detail above is the only copy the user reads.
        return Err(anyhow::anyhow!(incomplete_error(
            mcp_error.is_some(),
            hook_error.is_some()
        )));
    }

    // The engine steps all succeeded: now the companion. Not part of the numbered
    // steps above because it is the one that can fail without failing setup.
    let report = tokio::task::spawn_blocking(move || {
        let home = dirs::home_dir().unwrap_or_default();
        let engine = std::env::current_exe().unwrap_or_default();
        companion_step(
            companion,
            Path::new("claude"),
            &home,
            &engine,
            COMPANION_TIMEOUT,
        )
    })
    .await
    .unwrap_or_else(|e| {
        Some(RelayReport {
            outcome: CompanionOutcome::Failed {
                reason: format!("the install task did not finish: {e}"),
            },
            changed: false,
            steps: Vec::new(),
        })
    });

    eprintln!("\n=== Setup Complete ===\n");
    eprintln!("  Conversations: {}", conversations);
    eprintln!("  Reflections:   {}", reflections);
    eprintln!("  Projects:      {}", projects);
    eprintln!();
    eprint!("{}", next_steps(report.as_ref()));

    Ok(())
}

/// Run the hook install and keep its failure for the summary. The error is
/// flattened to one line; the remedy is printed once, by `incomplete_report`.
fn hook_install_step(install: impl FnOnce() -> Result<()>) -> Option<String> {
    match install() {
        Ok(()) => None,
        Err(e) => {
            eprintln!("  Warning: hook installation failed: {e}");
            Some(e.to_string())
        }
    }
}

/// The body of the "Setup Incomplete" summary: what failed, what still works,
/// and the commands that fix it. At least one of the two must have failed.
fn incomplete_report(mcp: Option<&McpFailure>, hook_error: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(error) = hook_error {
        out.push_str(&format!(
            "  Hook install failed: {error}\n  Hooks from an earlier install, if any, are unchanged. Run: csr-engine hook install --apply\n"
        ));
    }
    if let Some(failure) = mcp {
        let done = if hook_error.is_some() {
            "The import is done."
        } else {
            "Hooks and import are done."
        };
        out.push_str(&format!(
            "  {done} {}\n  {}\n",
            failure.summary, failure.detail
        ));
    } else {
        out.push_str("  The import and the MCP registration are done.\n");
    }
    out
}

/// The short error `handle` returns for an incomplete setup (non-zero exit).
fn incomplete_error(mcp_failed: bool, hooks_failed: bool) -> &'static str {
    match (mcp_failed, hooks_failed) {
        (true, true) => "MCP server not registered and hook install failed",
        (true, false) => "MCP server not registered",
        _ => "hook install failed",
    }
}

/// Hard stop for each `claude plugin` call. Only that child is killed.
const COMPANION_TIMEOUT: Duration = Duration::from_secs(120);

/// What `claude plugin` reports about the installed plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanionInfo {
    pub plugin: String,
    pub version: String,
    pub marketplace: String,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompanionOutcome {
    Installed(CompanionInfo),
    AlreadyInstalled(CompanionInfo),
    Failed { reason: String },
}

/// Captured result of one `claude` call.
struct ClaudeRun {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Run `claude <args>` with stdin closed, output captured and a hard timeout.
/// On timeout only the child we spawned is killed (and reaped); a grandchild that
/// keeps the pipes open is left alone and its output abandoned.
fn run_with_timeout(claude: &Path, args: &[&str], timeout: Duration) -> Result<ClaudeRun, String> {
    let shown = format!("claude plugin {}", args.get(1).copied().unwrap_or(""));
    let mut child = Command::new(claude)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run `{}`: {e}", claude.display()))?;

    fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            let _ = tx.send(buf);
        });
        rx
    }
    let out_rx = drain(child.stdout.take());
    let err_rx = drain(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`{shown}` did not finish within {} s and was stopped",
                    timeout.as_secs_f32().round()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("could not wait for `{shown}`: {e}"));
            }
        }
    };

    let grace = Duration::from_secs(2);
    let stdout = out_rx.recv_timeout(grace).unwrap_or_default();
    let stderr = err_rx.recv_timeout(grace).unwrap_or_default();
    Ok(ClaudeRun {
        success: status.success(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// The first JSON value in `text`: the whole text if it parses (`plugin list
/// --json` pretty-prints an array), else the first line that does.
fn first_json(
    text: &str,
    accept: impl Fn(&serde_json::Value) -> bool,
) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim()) {
        if accept(&v) {
            return Some(v);
        }
    }
    text.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .find(|v| accept(v))
}

/// `csr-companion@claude-self-reflect` -> ("csr-companion", "claude-self-reflect").
fn split_plugin_id(id: &str) -> (String, String) {
    match id.split_once('@') {
        Some((p, m)) => (p.to_string(), m.to_string()),
        None => (id.to_string(), "unknown".to_string()),
    }
}

fn info_from(entry: &serde_json::Value) -> Option<CompanionInfo> {
    let id = entry
        .get("id")
        .or_else(|| entry.get("pluginId"))
        .or_else(|| entry.get("plugin"))?
        .as_str()?;
    let (plugin, marketplace) = split_plugin_id(id);
    let text = |key: &str| entry.get(key).and_then(|v| v.as_str()).map(str::to_string);
    Some(CompanionInfo {
        plugin,
        version: text("version").unwrap_or_else(|| "unknown".to_string()),
        marketplace,
        scope: text("scope").unwrap_or_else(|| "unknown".to_string()),
    })
}

// ── the companion plugin: written by this binary, installed from a local marketplace ──

/// The plugin id Claude Code lists: `csr-companion@claude-self-reflect`.
fn relay_plugin_id() -> String {
    format!("{}@{}", emit::PLUGIN_NAME, emit::MARKETPLACE_NAME)
}

/// What `companion plugin install` and `setup` did, step by step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayReport {
    pub outcome: CompanionOutcome,
    /// The marketplace registration or the plugin changed, so open Claude Code sessions
    /// need `/reload-plugins`.
    pub changed: bool,
    /// One line per step that ran, in order.
    pub steps: Vec<String>,
}

pub const RELOAD_LINE: &str = "run /reload-plugins in open Claude Code sessions";

/// Where the marketplace Claude Code knows as `claude-self-reflect` stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MarketState {
    Absent,
    Here,
    /// Registered with another source: a GitHub repo, or some other folder.
    Elsewhere(String),
}

fn same_folder(a: &str, b: &Path) -> bool {
    let a = Path::new(a);
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Read `claude plugin marketplace list --json`.
fn market_state(list: &serde_json::Value, dir: &Path) -> MarketState {
    let Some(entry) = list
        .as_array()
        .and_then(|l| l.iter().find(|e| e["name"] == emit::MARKETPLACE_NAME))
    else {
        return MarketState::Absent;
    };
    let text = |key: &str| entry.get(key).and_then(|v| v.as_str());
    let path = text("path");
    if text("source") == Some("directory") && path.is_some_and(|p| same_folder(p, dir)) {
        return MarketState::Here;
    }
    let was = [text("source"), text("repo"), text("url"), path]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    MarketState::Elsewhere(was)
}

/// `claude <args>` that must exit 0. The error names the step and the first line of detail.
fn run_ok(
    claude: &Path,
    args: &[&str],
    timeout: Duration,
    what: &str,
) -> Result<ClaudeRun, String> {
    let run = run_with_timeout(claude, args, timeout)?;
    if run.success {
        return Ok(run);
    }
    let detail = first_json(&run.stdout, |v| v.is_object())
        .and_then(|v| {
            v.get("message")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| run.stderr.trim().to_string());
    Err(format!(
        "`claude plugin {what}` failed: {}",
        one_line(&detail)
    ))
}

/// `run_ok`, and a `--json` result that says `outcome: ok`.
fn run_outcome_ok(
    claude: &Path,
    args: &[&str],
    timeout: Duration,
    what: &str,
) -> Result<ClaudeRun, String> {
    let run = run_ok(claude, args, timeout, what)?;
    let Some(result) = first_json(&run.stdout, |v| v.is_object()) else {
        return Err(format!("`claude plugin {what}` returned no JSON result"));
    };
    if result.get("outcome").and_then(|o| o.as_str()) != Some("ok") {
        let message = result
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("outcome was not ok");
        return Err(format!(
            "`claude plugin {what}` did not succeed: {}",
            one_line(message)
        ));
    }
    Ok(run)
}

/// `claude plugin list --json` as entries.
fn list_plugins(claude: &Path, timeout: Duration) -> Result<Vec<serde_json::Value>, String> {
    let run = run_ok(claude, &["plugin", "list", "--json"], timeout, "list")?;
    first_json(&run.stdout, |v| v.is_array())
        .and_then(|v| v.as_array().cloned())
        .ok_or_else(|| "`claude plugin list` returned no JSON list".to_string())
}

fn relay_entry(list: &[serde_json::Value]) -> Option<&serde_json::Value> {
    let id = relay_plugin_id();
    list.iter()
        .find(|e| e.get("id").and_then(|v| v.as_str()) == Some(id.as_str()))
}

/// The cache holds this engine's version and the `register.js` this engine emits. A moved
/// binary changes `register.js` without a version bump, so the text counts as well.
fn relay_is_current(entry: &serde_json::Value, relay: &str) -> bool {
    let text = |key: &str| entry.get(key).and_then(|v| v.as_str());
    text("version") == Some(env!("CARGO_PKG_VERSION"))
        && text("installPath").is_some_and(|dir| {
            std::fs::read_to_string(Path::new(dir).join("hooks/register.js"))
                .is_ok_and(|cached| cached == relay)
        })
}

/// The options the user set on the plugin, as `(key, value)`. Removing a marketplace or
/// uninstalling a plugin deletes them, so they are read first and given back at install.
/// A read that fails, times out or prints no JSON object is an error, never an empty list:
/// an empty list would reinstall the plugin with the defaults over the user's choices. A
/// successful read of a plugin with nothing set is an empty list.
fn configured_options(claude: &Path, timeout: Duration) -> Result<Vec<(String, String)>, String> {
    let id = relay_plugin_id();
    let args = ["plugin", "configure", id.as_str(), "--json"];
    let run = run_ok(claude, &args, timeout, "configure")?;
    let info = first_json(&run.stdout, |v| v.is_object())
        .ok_or_else(|| "`claude plugin configure` returned no JSON result".to_string())?;
    Ok(info["configured"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|key| {
            let key = key.as_str()?;
            Some((key.to_string(), info["inputs"][key].as_str()?.to_string()))
        })
        .collect())
}

/// What the user had on the plugin before a step that deletes it: the options, and whether
/// it was switched on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    options: Vec<(String, String)>,
    enabled: bool,
}

impl Default for Snapshot {
    /// A plugin that was never there: no options, enabled (what an install gives).
    fn default() -> Self {
        Snapshot {
            options: Vec::new(),
            enabled: true,
        }
    }
}

/// Where a snapshot waits while the plugin is gone: it survives a migration that fails
/// half-way, so the rerun installs with the user's options and enabled state.
fn pending_snapshot_path(home: &Path) -> std::path::PathBuf {
    crate::companion::paths::Paths::new(home.to_path_buf())
        .keep
        .join("plugin-migration.json")
}

fn save_snapshot(home: &Path, snapshot: &Snapshot) -> Result<(), String> {
    let path = pending_snapshot_path(home);
    let options: Vec<[&str; 2]> = snapshot
        .options
        .iter()
        .map(|(k, v)| [k.as_str(), v.as_str()])
        .collect();
    let text = serde_json::json!({"options": options, "enabled": snapshot.enabled}).to_string();
    let root = path.parent().unwrap_or(home).to_path_buf();
    crate::companion::private::write_atomic(&path, &text, &root).map_err(|e| {
        format!(
            "could not save the plugin options to {}: {e}",
            path.display()
        )
    })
}

/// The snapshot a failed migration left. No file is `Ok(None)`. A file that cannot be read,
/// is not JSON or has a malformed `options` list is an error naming it: reading it as "no
/// snapshot" would install with the defaults and then delete the user's saved choices.
fn load_snapshot(home: &Path) -> Result<Option<Snapshot>, String> {
    let path = pending_snapshot_path(home);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(unreadable_snapshot(&path, &e.to_string())),
    };
    let bad = |why: &str| unreadable_snapshot(&path, why);
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| bad(&e.to_string()))?;
    let options = value["options"]
        .as_array()
        .ok_or_else(|| bad("`options` is not a list"))?
        .iter()
        .map(|pair| Some((pair[0].as_str()?.to_string(), pair[1].as_str()?.to_string())))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| bad("`options` holds an entry that is not a [key, value] pair"))?;
    let enabled = match &value["enabled"] {
        serde_json::Value::Null => true,
        serde_json::Value::Bool(b) => *b,
        _ => return Err(bad("`enabled` is not true or false")),
    };
    Ok(Some(Snapshot { options, enabled }))
}

fn unreadable_snapshot(path: &Path, why: &str) -> String {
    format!(
        "the saved plugin options in {} cannot be read ({}); fix the file or delete it, then \
         run `csr-engine companion plugin install` again",
        path.display(),
        one_line(why)
    )
}

fn clear_snapshot(home: &Path) {
    let _ = std::fs::remove_file(pending_snapshot_path(home));
}

/// Read the options and the enabled state of the installed plugin `entry`, and save them
/// where a rerun finds them, before the caller deletes the plugin. Any failure stops the
/// caller: nothing has been removed yet, and the error says how to retry.
fn snapshot_plugin(
    claude: &Path,
    home: &Path,
    entry: &serde_json::Value,
    timeout: Duration,
) -> Result<Snapshot, String> {
    let retry = "nothing was removed; run `csr-engine companion plugin install` to retry";
    let options = configured_options(claude, timeout)
        .map_err(|e| format!("could not read the plugin's options ({e}); {retry}"))?;
    let snapshot = Snapshot {
        options,
        enabled: relay_enabled(Some(entry)),
    };
    save_snapshot(home, &snapshot).map_err(|e| format!("{e}; {retry}"))?;
    Ok(snapshot)
}

/// `claude plugin install` with the saved options. An install enables the plugin, so one
/// the user had switched off (`enabled == false`) is disabled again.
fn install_relay_plugin(
    claude: &Path,
    options: &[(String, String)],
    enabled: bool,
    timeout: Duration,
) -> Result<(), String> {
    let id = relay_plugin_id();
    let pairs: Vec<String> = options.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let mut args = vec!["plugin", "install", id.as_str(), "--json"];
    for pair in &pairs {
        args.extend(["--config", pair.as_str()]);
    }
    run_outcome_ok(claude, &args, timeout, "install")?;
    if !enabled {
        disable_relay_plugin(claude, timeout)?;
    }
    Ok(())
}

/// `claude plugin disable`; a failure says how to finish by hand.
fn disable_relay_plugin(claude: &Path, timeout: Duration) -> Result<(), String> {
    let id = relay_plugin_id();
    let disable = ["plugin", "disable", id.as_str(), "--json"];
    run_outcome_ok(claude, &disable, timeout, "disable").map_err(|e| {
        format!("{e}; the plugin is installed and enabled, run `claude plugin disable {id}`")
    })?;
    Ok(())
}

/// Is the plugin listed as switched on? Unknown counts as on.
fn relay_enabled(entry: Option<&serde_json::Value>) -> bool {
    entry.and_then(|e| e["enabled"].as_bool()).unwrap_or(true)
}

struct RelayState {
    steps: Vec<String>,
    changed: bool,
    /// The plugin step installed, updated or reinstalled something.
    plugin_changed: bool,
}

fn relay_steps(
    claude: &Path,
    home: &Path,
    engine: &Path,
    timeout: Duration,
    st: &mut RelayState,
) -> Result<CompanionInfo, String> {
    // a. the marketplace, written from this binary
    let market = emit::write_marketplace(home, Some(engine))
        .map_err(|e| format!("could not write the marketplace: {e:#}"))?;
    st.steps
        .push(format!("marketplace written: {}", market.dir.display()));
    let dir = market
        .dir
        .to_str()
        .ok_or_else(|| "the marketplace folder is not UTF-8".to_string())?;

    // b. the registration
    let run = run_ok(
        claude,
        &["plugin", "marketplace", "list", "--json"],
        timeout,
        "marketplace list",
    )?;
    let list = first_json(&run.stdout, |v| v.is_array())
        .ok_or_else(|| "`claude plugin marketplace list` returned no JSON list".to_string())?;
    let add = ["plugin", "marketplace", "add", dir];
    match market_state(&list, &market.dir) {
        MarketState::Here => st.steps.push("marketplace already registered".into()),
        MarketState::Absent => {
            run_ok(claude, &add, timeout, "marketplace add")?;
            st.changed = true;
            st.steps.push("marketplace registered".into());
        }
        MarketState::Elsewhere(was) => {
            // `add` over the same name repoints it and keeps the plugin and its options.
            // Older Claude Code may refuse; then remove, add and give the options back.
            if let Err(refused) = run_ok(claude, &add, timeout, "marketplace add") {
                // Removing the marketplace uninstalls the plugin and drops its options, so
                // they are read and saved first. A plugin that is not installed has none.
                let listed = list_plugins(claude, timeout)?;
                if let Some(entry) = relay_entry(&listed) {
                    snapshot_plugin(claude, home, entry, timeout)
                        .map_err(|e| format!("{refused}; {e}"))?;
                }
                st.changed = true;
                let remove = ["plugin", "marketplace", "remove", emit::MARKETPLACE_NAME];
                run_ok(claude, &remove, timeout, "marketplace remove")
                    .map_err(|e| format!("{refused}; {e}"))?;
                run_ok(claude, &add, timeout, "marketplace add")?;
            }
            st.changed = true;
            st.steps
                .push(format!("marketplace moved here (was: {was})"));
        }
    }

    // c. the plugin
    let id = relay_plugin_id();
    let version = env!("CARGO_PKG_VERSION");
    // A migration that failed half-way left the user's options here. A file that cannot be
    // read stops the step before anything is installed or deleted.
    let pending = load_snapshot(home)?;
    let listed = list_plugins(claude, timeout)?;
    match relay_entry(&listed) {
        None => {
            let Snapshot { options, enabled } = pending.clone().unwrap_or_default();
            install_relay_plugin(claude, &options, enabled, timeout)?;
            st.changed = true;
            st.plugin_changed = true;
            st.steps.push(format!("plugin installed: {version}"));
        }
        Some(entry) if relay_is_current(entry, &market.relay) => {
            st.steps.push(format!("plugin already current: {version}"));
        }
        Some(entry) => {
            let was = entry["version"].as_str().unwrap_or("unknown").to_string();
            let update = ["plugin", "update", id.as_str(), "--json"];
            run_outcome_ok(claude, &update, timeout, "update")?;
            st.changed = true;
            st.plugin_changed = true;
            let refreshed = list_plugins(claude, timeout)?;
            if relay_entry(&refreshed).is_some_and(|e| relay_is_current(e, &market.relay)) {
                st.steps.push(format!("plugin updated: {was} -> {version}"));
            } else {
                // The update kept the old cache. Uninstall and install put the new one in.
                let Snapshot { options, enabled } = snapshot_plugin(claude, home, entry, timeout)?;
                let uninstall = ["plugin", "uninstall", id.as_str(), "--keep-data", "--json"];
                run_outcome_ok(claude, &uninstall, timeout, "uninstall")?;
                install_relay_plugin(claude, &options, enabled, timeout)?;
                st.steps
                    .push(format!("plugin reinstalled: {was} -> {version}"));
            }
        }
    }

    let mut listed = list_plugins(claude, timeout)?;
    let after = || format!("`claude plugin list` does not show {id} after the install");
    let entry = relay_entry(&listed).ok_or_else(after)?;
    if !relay_is_current(entry, &market.relay) {
        return Err(format!(
            "{id} is installed but its cache is not this engine's plugin; \
             run `claude plugin uninstall {id}` and then this again"
        ));
    }
    // A migration that failed after its install but before the disable left the plugin
    // current and enabled, with the user's "off" still in the file. The saved options need
    // no such step: the install that ran with `--config` applied them.
    if pending.is_some_and(|s| !s.enabled) && relay_enabled(Some(entry)) {
        disable_relay_plugin(claude, timeout)?;
        st.changed = true;
        st.plugin_changed = true;
        st.steps
            .push("plugin disabled again (as it was before)".into());
        listed = list_plugins(claude, timeout)?;
        if relay_entry(&listed).is_none_or(|e| relay_enabled(Some(e))) {
            return Err(format!(
                "`claude plugin disable` ran but {id} is still listed as enabled; \
                 run `claude plugin disable {id}`"
            ));
        }
    }
    let entry = relay_entry(&listed).ok_or_else(after)?;
    // The install is confirmed: the saved state has done its job.
    clear_snapshot(home);
    info_from(entry).ok_or_else(|| "`claude plugin list` named no plugin".to_string())
}

/// Write the local marketplace and install the plugin from it through Claude Code's plugin
/// CLI. `engine` is the binary the relay will start. Never panics and never returns an
/// error: a failure is a `Failed` outcome with the steps that ran before it.
pub fn install_relay(claude: &Path, home: &Path, engine: &Path, timeout: Duration) -> RelayReport {
    let mut st = RelayState {
        steps: Vec::new(),
        changed: false,
        plugin_changed: false,
    };
    let outcome = match relay_steps(claude, home, engine, timeout, &mut st) {
        Ok(info) if st.plugin_changed => CompanionOutcome::Installed(info),
        Ok(info) => CompanionOutcome::AlreadyInstalled(info),
        Err(reason) => CompanionOutcome::Failed { reason },
    };
    RelayReport {
        outcome,
        changed: st.changed,
        steps: st.steps,
    }
}

/// The companion step of setup. `enabled == false` (`--no-companion`) makes no `claude`
/// call and writes nothing.
fn companion_step(
    enabled: bool,
    claude: &Path,
    home: &Path,
    engine: &Path,
    timeout: Duration,
) -> Option<RelayReport> {
    if !enabled {
        return None;
    }
    eprintln!("Installing the csr-companion plugin from this binary through `claude plugin`...");
    let report = install_relay(claude, home, engine, timeout);
    for step in &report.steps {
        eprintln!("  {step}");
    }
    Some(report)
}

/// What `csr-engine companion plugin install` prints; the exit code is non-zero on failure.
pub fn relay_install_command() -> i32 {
    let (Some(home), Ok(engine)) = (dirs::home_dir(), std::env::current_exe()) else {
        eprintln!("companion: cannot find the home folder or this binary");
        return 1;
    };
    let report = install_relay(Path::new("claude"), &home, &engine, COMPANION_TIMEOUT);
    for step in &report.steps {
        println!("{step}");
    }
    if report.changed {
        println!("{RELOAD_LINE}");
    }
    if let CompanionOutcome::Failed { reason } = &report.outcome {
        eprintln!("companion: not installed: {reason}");
        return 1;
    }
    0
}

fn one_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let line = line.trim();
    if line.is_empty() {
        "no detail from claude".to_string()
    } else {
        line.chars().take(240).collect()
    }
}

fn next_steps_text(companion_text: &str) -> String {
    format!(
        "Next steps:\n  1. Restart Claude Code to activate MCP tools\n  2. Open Claude Code in a project you have worked in; its first line recaps where you left off\n  3. Ask in plain words, e.g. \"what did we work on last week?\"\n\n{companion_text}"
    )
}

/// What a successful setup ends with: the next steps, then the companion's receipt,
/// `/reload-plugins` when something changed, and the one command that retries the install. `None`
/// is `--no-companion`.
fn next_steps(report: Option<&RelayReport>) -> String {
    let retry = "To install it, run: csr-engine companion plugin install\n";
    let companion_text = match report.map(|r| (&r.outcome, r.changed)) {
        Some((CompanionOutcome::Installed(i) | CompanionOutcome::AlreadyInstalled(i), changed)) => {
            let reload = if changed {
                format!("{RELOAD_LINE}\n")
            } else {
                String::new()
            };
            format!(
                "companion: {} {} from {}, scope {}\n{reload}",
                i.plugin, i.version, i.marketplace, i.scope
            )
        }
        Some((CompanionOutcome::Failed { reason }, changed)) => {
            // A step that failed after something was removed still changed what is loaded.
            let reload = if changed {
                format!("{RELOAD_LINE}\n")
            } else {
                String::new()
            };
            format!("companion: not installed: {reason}\n{retry}{reload}")
        }
        None => format!("companion: skipped (--no-companion)\n{retry}"),
    };
    next_steps_text(&companion_text)
}

/// Is this run allowed to ask an interactive question? `CSR_AUTO_SETUP=1`
/// (the installer's automation path) and a non-tty stdin both mean "take the
/// default without prompting". `CSR_SKIP_SETUP` belongs to `install.sh` and
/// is never reached here — if it were set, setup would not be running.
fn interactive() -> bool {
    if std::env::var("CSR_AUTO_SETUP")
        .map(|v| v.trim() == "1")
        .unwrap_or(false)
    {
        return false;
    }
    std::io::stdin().is_terminal()
}

/// The estimate + toggle screen. Returns the recorded decision.
///
/// Honesty rules: the number shown is computed from the measured candidate
/// count in the corpus and the configured tier, it is labelled an estimate
/// and states its assumptions, and it is printed BEFORE the question. A
/// non-interactive run takes the documented default (ON) and says so rather
/// than silently assuming consent.
fn dreaming_consent_step(storage: &Storage) -> Result<bool> {
    let tier = policy::effort_tier();
    let estimate = night_estimate(storage, tier);

    eprintln!("[7/8] Dreaming (overnight review of your open work)");
    eprintln!("  While Claude Code is idle, CSR re-checks open todos and blockers");
    eprintln!("  against the code as it stands now, and writes what it can prove.");
    eprintln!("  Cost: {}", estimate.label(tier));
    eprintln!(
        "  Effort tier '{}' ({} reasoning, up to {} episodes per pass) — change with CSR_DREAM_EFFORT=less|balanced|max.",
        tier.as_str(),
        tier.reasoning_effort(),
        tier.episodes_per_pass(),
    );

    let granted = if interactive() {
        ask_yes_default_yes("  Enable dreaming? [Y/n] ")
    } else {
        eprintln!("  Non-interactive run — keeping the default (enabled).");
        true
    };

    dream_cadence::record_consent(storage, granted)?;
    if granted {
        eprintln!("  Dreaming enabled. Turn it off any time with CSR_NO_DREAMING=1.");
    } else {
        eprintln!("  Dreaming declined; the daemon will not run night passes.");
    }
    Ok(granted)
}

/// The per-night estimate for `tier`, from the corpus's measured candidate
/// count and the configured cadence/budget.
fn night_estimate(storage: &Storage, tier: EffortTier) -> NightEstimate {
    policy::estimate_night(
        crate::dream::threads::count_candidate_episodes(storage),
        tier,
        policy::budget_cap(tier),
        dream_cadence::interval_secs(),
    )
}

/// Read one line; anything but an explicit "n"/"no" keeps the default (yes).
fn ask_yes_default_yes(prompt: &str) -> bool {
    eprint!("{prompt}");
    use std::io::Write;
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    if std::io::stdin().lock().read_line(&mut answer).is_err() {
        return true;
    }
    !matches!(answer.trim().to_ascii_lowercase().as_str(), "n" | "no")
}

/// The MCP server name we own in Claude Code's user-scope config.
const MCP_SERVER_NAME: &str = "claude-self-reflect";

/// What a failed `claude mcp add` means.
#[derive(Debug, PartialEq, Eq)]
enum AddFailure {
    /// The name is already taken — by an older copy of this binary at a
    /// different absolute path, typically after an npm or install.sh upgrade.
    /// `claude mcp add` has no overwrite flag, so the entry must be removed
    /// before the new command can be registered.
    AlreadyRegistered,
    /// Anything else. Reported, never papered over.
    Fatal,
}

fn classify_add_failure(stderr: &str) -> AddFailure {
    if stderr.to_lowercase().contains("already exists") {
        AddFailure::AlreadyRegistered
    } else {
        AddFailure::Fatal
    }
}

fn mcp_add(binary_str: &str) -> std::io::Result<std::process::Output> {
    std::process::Command::new("claude")
        .args(["mcp", "add", MCP_SERVER_NAME, binary_str, "-s", "user"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
}

fn mcp_remove() -> std::io::Result<std::process::Output> {
    std::process::Command::new("claude")
        .args(["mcp", "remove", MCP_SERVER_NAME, "-s", "user"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
}

/// What happened to the registration that was there before we touched it.
#[derive(Debug, PartialEq, Eq)]
enum PriorEntry {
    /// Nothing of ours is registered, as far as we can tell.
    None,
    /// An entry is still registered under our name, exactly as we found it —
    /// we either never removed it or refused to. `Some` when we could read the
    /// command it points at.
    Intact(Option<String>),
    /// Removed and put back. `exact` is false when only the stdio command and
    /// args could be replayed, so `env` and any other fields are gone.
    Restored {
        command: Option<String>,
        exact: bool,
    },
    /// We removed ours and, by the time we tried again, the name was taken:
    /// something else registered it in between. An entry is present, but it is
    /// not the one we captured.
    Occupied(Option<String>),
    /// Removed, and putting it back failed. The user is unregistered and has to
    /// be told exactly that — plus the snapshot, so they can replay it.
    Lost {
        command: Option<String>,
        snapshot: String,
    },
}

impl PriorEntry {
    /// True when an entry under our name is present right now — in which case
    /// a bare `claude mcp add` would just fail with "already exists" again.
    fn still_registered(&self) -> bool {
        matches!(
            self,
            PriorEntry::Intact(_) | PriorEntry::Restored { .. } | PriorEntry::Occupied(_)
        )
    }
}

/// A registration failure, split into the one-line banner and the explanation.
struct McpFailure {
    summary: String,
    detail: String,
}

/// The message a human gets when we could not register the server.
///
/// The commands are state-specific: an entry that is still present has to be
/// removed before an add can succeed, and telling someone to run an add that
/// will fail with "already exists" is worse than saying nothing. The path is
/// shell-quoted, and what happened to whatever was registered before is always
/// stated — silently deleting a working registration would be worse than not
/// upgrading at all.
fn mcp_registration_error(binary_str: &str, stderr: &str, prior: &PriorEntry) -> McpFailure {
    let quoted = crate::shell::shell_quote(binary_str);

    let mut detail = format!("could not register the MCP server: {}", stderr.trim());
    detail.push_str("\n  Run this yourself, then restart Claude Code:");
    if prior.still_registered() {
        detail.push_str(&format!(
            "\n    claude mcp remove {MCP_SERVER_NAME} -s user"
        ));
    }
    detail.push_str(&format!(
        "\n    claude mcp add {MCP_SERVER_NAME} {quoted} -s user"
    ));

    // "(/old/csr-engine)" when we know what it pointed at, nothing when we do
    // not — an unreadable snapshot or an SSE/HTTP entry has no binary to name.
    let named = |command: &Option<String>| match command {
        Some(c) => format!(" ({c})"),
        None => String::new(),
    };

    let summary = match prior {
        PriorEntry::None => "The MCP server is NOT registered:".to_string(),
        PriorEntry::Intact(old) => {
            detail.push_str("\n  The previous MCP entry was left exactly as it was.");
            format!(
                "The MCP server is still registered to the previous MCP entry{}, not the new one:",
                named(old)
            )
        }
        PriorEntry::Restored { command, exact } => {
            if *exact {
                detail.push_str(&format!(
                    "\n  The previous registration{} was put back unchanged, so Claude Code still works — with the old entry.",
                    named(command)
                ));
            } else {
                detail.push_str(&format!(
                    "\n  The previous registration{} was put back from its command and args only; any env vars it had were not restored.",
                    named(command)
                ));
            }
            format!(
                "The MCP server is still registered to the previous MCP entry{}, not the new one:",
                named(command)
            )
        }
        PriorEntry::Occupied(current) => {
            detail.push_str(&format!(
                "\n  While setup was running, something else registered the name{}. Ours was not restored over it.",
                named(current)
            ));
            format!(
                "The MCP server name is held by another registration{}, not the new one:",
                named(current)
            )
        }
        PriorEntry::Lost { command, snapshot } => {
            detail.push_str(&format!(
                "\n  The previous registration{} was removed and could not be restored. Nothing is registered right now.",
                named(command)
            ));
            detail.push_str(&format!(
                "\n  To put it back exactly as it was:\n    claude mcp add-json {MCP_SERVER_NAME} {} -s user",
                crate::shell::shell_quote(snapshot)
            ));
            "The MCP server is NOT registered:".to_string()
        }
    };

    McpFailure { summary, detail }
}

/// The whole `mcpServers["claude-self-reflect"]` object from `~/.claude.json`.
///
/// Read directly rather than through `claude mcp get` so a broken CLI cannot
/// hide it, and kept whole so a restore can replay `args` and `env` rather than
/// just the command. One non-blocking open, metadata taken from that same
/// handle, and a capped read from it — checking a pathname and then reopening
/// it would let the file grow past the cap or turn into a FIFO in between.
/// Fail-open: any problem yields None, and None means we refuse to remove.
fn current_mcp_entry() -> Option<serde_json::Value> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    const MAX_CONFIG_BYTES: u64 = 32 * 1024 * 1024;

    let path = dirs::home_dir()?.join(".claude.json");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;

    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES {
        return None;
    }

    let mut content = String::new();
    file.by_ref()
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut content)
        .ok()?;
    if content.len() as u64 > MAX_CONFIG_BYTES {
        return None;
    }

    let config: serde_json::Value = serde_json::from_str(&content).ok()?;
    config.get("mcpServers")?.get(MCP_SERVER_NAME).cloned()
}

/// The `command` inside a captured entry, for display.
fn entry_command(entry: &serde_json::Value) -> Option<String> {
    entry.get("command")?.as_str().map(|s| s.to_string())
}

/// Register csr-engine as an MCP server with Claude Code.
///
/// An upgrade installs a new binary at a new absolute path while the user-scope
/// registration still names the old one, and `claude mcp add` refuses to
/// overwrite it. Failing quietly there is how setup used to print "Setup
/// Complete" while Claude Code kept launching the binary that was just
/// replaced. So: snapshot the old entry, remove, add once, and put the snapshot
/// back if that fails. Without a snapshot we do not remove at all.
fn register_mcp_server() -> Result<(), McpFailure> {
    let binary_path = std::env::current_exe().map_err(|e| {
        mcp_registration_error(
            "csr-engine",
            &format!("cannot determine our own path: {e}"),
            &PriorEntry::None,
        )
    })?;
    let binary_str = binary_path.to_string_lossy().to_string();

    let output = match mcp_add(&binary_str) {
        Ok(output) => output,
        Err(e) => {
            // `claude` could not be spawned at all. There used to be a fallback
            // here that wrote `mcpServers` into ~/.claude/settings.json, which
            // Claude Code does not read for MCP — it reported success while
            // registering nothing.
            return Err(mcp_registration_error(
                &binary_str,
                &format!("could not run `claude`: {e}"),
                &PriorEntry::None,
            ));
        }
    };

    if output.status.success() {
        eprintln!("  Registered via `claude mcp add`");
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if classify_add_failure(&stderr) == AddFailure::Fatal {
        return Err(mcp_registration_error(
            &binary_str,
            &stderr,
            &PriorEntry::None,
        ));
    }

    // "already exists", so there IS an entry. Snapshot it before removing:
    // without a copy we cannot put it back, and removing something we cannot
    // restore is the one outcome worse than not upgrading.
    let Some(previous) = current_mcp_entry() else {
        return Err(mcp_registration_error(
            &binary_str,
            "a registration already exists but ~/.claude.json could not be read, so it was left alone rather than removed",
            &PriorEntry::Intact(None),
        ));
    };
    let previous_command = entry_command(&previous);
    eprintln!("  Existing registration found — repointing it at {binary_str}");

    match mcp_remove() {
        Ok(ref out) if out.status.success() => {}
        Ok(out) => {
            // The old entry is still in place: report and stop.
            return Err(mcp_registration_error(
                &binary_str,
                &format!(
                    "`claude mcp remove` failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                ),
                &PriorEntry::Intact(previous_command),
            ));
        }
        Err(e) => {
            return Err(mcp_registration_error(
                &binary_str,
                &format!("could not run `claude mcp remove`: {e}"),
                &PriorEntry::Intact(previous_command),
            ));
        }
    }

    // Exactly one retry: a second failure is a real problem, not a race.
    let retry_stderr = match mcp_add(&binary_str) {
        Ok(retry) if retry.status.success() => {
            eprintln!("  Registered via `claude mcp add`");
            return Ok(());
        }
        Ok(retry) => String::from_utf8_lossy(&retry.stderr).into_owned(),
        Err(e) => format!("could not run `claude mcp add`: {e}"),
    };

    // The name was free when we removed ours, so "already exists" here means
    // something else claimed it in between. Putting our snapshot back would
    // fail the same way, and an entry IS present — say so instead.
    let prior = if classify_add_failure(&retry_stderr) == AddFailure::AlreadyRegistered {
        PriorEntry::Occupied(current_mcp_entry().as_ref().and_then(entry_command))
    } else {
        restore_previous(&previous)
    };

    Err(mcp_registration_error(&binary_str, &retry_stderr, &prior))
}

/// How a `claude mcp …` run ended.
#[derive(Debug, PartialEq, Eq)]
enum ClaudeOutcome {
    Succeeded,
    /// The name is taken. During a restore that means somebody else got there
    /// first, which is a different situation from a plain failure.
    AlreadyRegistered,
    Failed,
}

fn run_claude(args: &[String]) -> ClaudeOutcome {
    match std::process::Command::new("claude")
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
    {
        Ok(out) if out.status.success() => ClaudeOutcome::Succeeded,
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if classify_add_failure(&stderr) == AddFailure::AlreadyRegistered {
                ClaudeOutcome::AlreadyRegistered
            } else {
                ClaudeOutcome::Failed
            }
        }
        Err(_) => ClaudeOutcome::Failed,
    }
}

/// The legacy `claude mcp add` argv that reproduces a snapshot, or None when
/// this form cannot express it.
///
/// Only stdio entries qualify: an SSE or HTTP registration needs `--transport`
/// and a URL, and inventing an executable name for one would be a lie. The
/// `--` matters — Claude Code's own help requires it before a subprocess
/// command, and without it an ordinary `args: ["--serve"]` is parsed as a flag
/// to `claude` itself and the restore fails with the entry already deleted.
fn legacy_restore_args(previous: &serde_json::Value) -> Option<Vec<String>> {
    // A missing field is the historical stdio shape; a present field of the
    // wrong type is an entry this form cannot express, not one to guess at.
    match previous.get("type") {
        None => {}
        Some(t) if t.as_str() == Some("stdio") => {}
        Some(_) => return None,
    }
    let command = previous.get("command")?.as_str()?.to_string();

    let mut args = vec![
        "mcp".to_string(),
        "add".to_string(),
        "-s".to_string(),
        "user".to_string(),
        MCP_SERVER_NAME.to_string(),
        "--".to_string(),
        command,
    ];
    if let Some(extra) = previous.get("args") {
        for value in extra.as_array()? {
            // A non-string argument cannot be replayed on a command line, so
            // the whole fallback is off rather than silently lossy.
            args.push(value.as_str()?.to_string());
        }
    }
    Some(args)
}

/// Put back the entry we removed, whole.
///
/// `claude mcp add-json` replays the captured object exactly — `args`, `env`
/// and anything else Claude Code stored. Older Claude Code releases have no
/// `add-json`, so there is one fallback for stdio entries that loses `env`;
/// the message says so. Anything else is reported as lost, with the snapshot
/// printed so the user can replay it by hand. One attempt each, and the result
/// is reported either way since the caller is already returning an error.
fn restore_previous(previous: &serde_json::Value) -> PriorEntry {
    let command = entry_command(previous);
    let snapshot = serde_json::to_string(previous).ok();

    if let Some(json) = &snapshot {
        let args = [
            "mcp".to_string(),
            "add-json".to_string(),
            MCP_SERVER_NAME.to_string(),
            json.clone(),
            "-s".to_string(),
            "user".to_string(),
        ];
        match run_claude(&args) {
            ClaudeOutcome::Succeeded => {
                return PriorEntry::Restored {
                    command,
                    exact: true,
                }
            }
            ClaudeOutcome::AlreadyRegistered => {
                return PriorEntry::Occupied(current_mcp_entry().as_ref().and_then(entry_command))
            }
            ClaudeOutcome::Failed => {}
        }
    }

    if let Some(args) = legacy_restore_args(previous) {
        match run_claude(&args) {
            ClaudeOutcome::Succeeded => {
                return PriorEntry::Restored {
                    command,
                    exact: false,
                }
            }
            ClaudeOutcome::AlreadyRegistered => {
                return PriorEntry::Occupied(current_mcp_entry().as_ref().and_then(entry_command))
            }
            ClaudeOutcome::Failed => {}
        }
    }

    PriorEntry::Lost {
        command,
        snapshot: snapshot.unwrap_or_else(|| previous.to_string()),
    }
}

/// Save the Anthropic API key to ~/.claude-self-reflect/.env
fn save_anthropic_key(csr_dir: &Path, key: &str) -> Result<()> {
    let env_path = csr_dir.join(".env");

    // Read existing .env content if it exists
    let mut content = if env_path.exists() {
        let existing = std::fs::read_to_string(&env_path)?;
        // Remove existing ANTHROPIC_API_KEY lines
        existing
            .lines()
            .filter(|line| !line.starts_with("ANTHROPIC_API_KEY="))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        String::new()
    };

    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&format!("ANTHROPIC_API_KEY={}\n", key));

    // Atomic write
    let tmp_path = env_path.with_extension("env.tmp");
    std::fs::write(&tmp_path, &content)?;
    std::fs::rename(&tmp_path, &env_path)?;

    Ok(())
}

/// Count total JSONL files across all project directories.
fn count_total_files(projects_dir: &Path) -> usize {
    let projects = match import::discover_projects(projects_dir) {
        Ok(p) => p,
        Err(_) => return 0,
    };
    let mut count = 0;
    for (dir, _) in &projects {
        if let Ok(files) = import::list_conversation_jsonl_files(dir) {
            count += files.len();
        }
    }
    count
}

// ---------------------------------------------------------------------------
// The background daemon (launchd agent)
// ---------------------------------------------------------------------------

/// The launchd label the daemon runs under. `scripts/launchd/`, the installers
/// (`kickstart`) and `vector_ipc::launchd_database` all name this file.
pub const DAEMON_LABEL: &str = "com.ramakay.csr-engine.daemon";

/// First line of a plist this module wrote. A plist without it was written by a
/// person (the manual steps older docs describe) and is left alone while its
/// binary still exists.
const DAEMON_PLIST_MARKER: &str = "<!-- Written by `csr-engine setup`; setup rewrites this file when the install changes. Hand edits belong in a plist without this line. -->";

/// Everything the agent plist depends on. Plain data, so the rendering is a pure
/// function and a test never touches the real `~/Library/LaunchAgents`.
#[derive(Debug, Clone)]
struct DaemonAgentSpec {
    /// Absolute path of the running engine binary.
    engine: std::path::PathBuf,
    home: std::path::PathBuf,
    db_path: std::path::PathBuf,
    projects_dir: std::path::PathBuf,
    /// Where `claude` resolved when setup ran, if anywhere. The daemon's
    /// narratives and dream passes run it, and launchd starts with a bare PATH.
    claude_dir: Option<std::path::PathBuf>,
    /// Setup itself ran with `CSR_NO_CODEX_IMPORT=1`; the daemon keeps that choice.
    no_codex_import: bool,
}

impl DaemonAgentSpec {
    fn plist_path(&self) -> std::path::PathBuf {
        self.home
            .join("Library/LaunchAgents")
            .join(format!("{DAEMON_LABEL}.plist"))
    }

    fn data_dir(&self) -> std::path::PathBuf {
        self.home.join(".claude-self-reflect")
    }

    /// PATH for the daemon: where `claude` is, where this binary is, then the
    /// usual places. Any `CSR_NO_*` key in the plist shows up in the pane as an
    /// inhibited feature, so the only one written is the one setup was given.
    fn path_env(&self) -> String {
        let mut dirs: Vec<std::path::PathBuf> = Vec::new();
        dirs.extend(self.claude_dir.clone());
        dirs.extend(self.engine.parent().map(Path::to_path_buf));
        dirs.push(self.home.join(".local/bin"));
        for fixed in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
            dirs.push(fixed.into());
        }
        let mut seen = std::collections::HashSet::new();
        dirs.retain(|d| seen.insert(d.clone()));
        dirs.iter()
            .map(|d| d.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(":")
    }
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The agent plist. Every path is absolute. The database and projects folder are
/// always passed explicitly, because `vector_ipc::launchd_database` reads the
/// database the daemon serves from `--db-path`.
fn render_daemon_plist(spec: &DaemonAgentSpec) -> String {
    let s = |p: &Path| xml_escape(&p.to_string_lossy());
    let data_dir = spec.data_dir();
    let codex = if spec.no_codex_import {
        "    <key>CSR_NO_CODEX_IMPORT</key>\n    <string>1</string>\n"
    } else {
        ""
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
{DAEMON_PLIST_MARKER}
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{DAEMON_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{engine}</string>
    <string>--db-path</string>
    <string>{db}</string>
    <string>--projects-dir</string>
    <string>{projects}</string>
    <string>daemon</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
{codex}    <key>PATH</key>
    <string>{path}</string>
  </dict>
  <key>WorkingDirectory</key>
  <string>{data}</string>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ThrottleInterval</key>
  <integer>30</integer>
  <key>ProcessType</key>
  <string>Background</string>
  <key>Nice</key>
  <integer>10</integer>
  <key>StandardOutPath</key>
  <string>{out}</string>
  <key>StandardErrorPath</key>
  <string>{err}</string>
</dict>
</plist>
"#,
        engine = s(&spec.engine),
        db = s(&spec.db_path),
        projects = s(&spec.projects_dir),
        path = xml_escape(&spec.path_env()),
        data = s(&data_dir),
        out = s(&data_dir.join("logs/daemon.out.log")),
        err = s(&data_dir.join("logs/daemon.err.log")),
    )
}

/// The first `ProgramArguments` string of a plist: the binary launchd would run.
fn plist_program(text: &str) -> Option<&str> {
    let rest = &text[text.find("<key>ProgramArguments</key>")?..];
    let rest = &rest[rest.find("<string>")? + "<string>".len()..];
    Some(&rest[..rest.find("</string>")?])
}

/// What the daemon step did, for the one summary line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonOutcome {
    /// `CSR_NO_DAEMON` is set: nothing was written or started.
    OptedOut,
    /// Not macOS: nothing was written; the run command is printed.
    Manual,
    /// The agent is installed and launchd was told to run it.
    Running { rewritten: bool },
    /// An agent somebody wrote by hand was kept and started.
    KeptExisting,
    /// A step failed; the text is the exact reason.
    Failed(String),
}

/// One `launchctl` call: `Ok(())` when it exited 0, otherwise its stderr.
type LaunchctlRun<'a> = dyn FnMut(&[&str]) -> std::result::Result<(), String> + 'a;

/// `CSR_NO_DAEMON=1` (or `true`) is the opt-out.
fn daemon_opted_out() -> bool {
    std::env::var("CSR_NO_DAEMON").is_ok_and(|v| matches!(v.trim(), "1" | "true"))
}

fn real_launchctl(args: &[&str]) -> std::result::Result<(), String> {
    let out = Command::new("launchctl")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run launchctl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let detail = String::from_utf8_lossy(&out.stderr);
        Err(format!(
            "launchctl {} failed ({}): {}",
            args.join(" "),
            out.status,
            detail.trim()
        ))
    }
}

/// Write the agent plist when it differs, load it, and make sure the daemon runs.
///
/// Idempotent: an identical plist is not rewritten. A changed one is booted out
/// first, because `kickstart` does not re-read the file. `bootstrap` right after
/// `bootout` can fail while launchd tears the old job down, so it is retried.
/// A job that is already loaded and unchanged is restarted with `kickstart -k`
/// so a new binary build is the one running. A fresh `bootstrap` starts the job
/// itself (`RunAtLoad`), so it gets no second start.
fn install_daemon_agent(
    spec: &DaemonAgentSpec,
    uid: u32,
    launchctl: &mut LaunchctlRun,
    pause: &dyn Fn(Duration),
) -> DaemonOutcome {
    let plist = spec.plist_path();
    let wanted = render_daemon_plist(spec);
    let existing = std::fs::read_to_string(&plist).ok();

    // A hand-written agent whose binary still exists is the user's. It is
    // started if it is not loaded, never rewritten.
    let foreign = existing.as_deref().is_some_and(|text| {
        !text.contains(DAEMON_PLIST_MARKER)
            && plist_program(text).is_some_and(|program| Path::new(program).exists())
    });
    let rewrite = !foreign && existing.as_deref() != Some(wanted.as_str());

    let domain = format!("gui/{uid}");
    let target = format!("{domain}/{DAEMON_LABEL}");
    let plist_arg = plist.to_string_lossy().into_owned();
    let loaded = launchctl(&["print", &target]).is_ok();

    if rewrite {
        if let Err(e) = std::fs::create_dir_all(spec.data_dir().join("logs")) {
            return DaemonOutcome::Failed(format!("could not create the log folder: {e}"));
        }
        if let Some(dir) = plist.parent() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                return DaemonOutcome::Failed(format!("could not create {}: {e}", dir.display()));
            }
        }
        let tmp = plist.with_extension("plist.tmp");
        if let Err(e) = std::fs::write(&tmp, &wanted).and_then(|()| std::fs::rename(&tmp, &plist)) {
            let _ = std::fs::remove_file(&tmp);
            return DaemonOutcome::Failed(format!("could not write {}: {e}", plist.display()));
        }
        if loaded {
            let _ = launchctl(&["bootout", &target]);
            pause(Duration::from_millis(500));
        }
    } else if loaded {
        // Unchanged and loaded: restart onto the current build.
        return match launchctl(&["kickstart", "-k", &target]) {
            Ok(()) if foreign => DaemonOutcome::KeptExisting,
            Ok(()) => DaemonOutcome::Running { rewritten: false },
            Err(e) => DaemonOutcome::Failed(e),
        };
    }

    // Not loaded (fresh, or just booted out): load it, which starts it.
    let mut last = String::new();
    for attempt in 0..6 {
        if attempt > 0 {
            pause(Duration::from_millis(500));
        }
        match launchctl(&["bootstrap", &domain, &plist_arg]) {
            Ok(()) => {
                return if foreign {
                    DaemonOutcome::KeptExisting
                } else {
                    DaemonOutcome::Running { rewritten: rewrite }
                };
            }
            Err(e) => last = e,
        }
    }
    DaemonOutcome::Failed(last)
}

/// The daemon step of setup. Never fails setup: the daemon is what keeps dreams
/// and enrichment going, but every other part of the install works without it.
fn daemon_step(
    spec: &DaemonAgentSpec,
    opted_out: bool,
    macos: bool,
    uid: u32,
    launchctl: &mut LaunchctlRun,
    pause: &dyn Fn(Duration),
) -> DaemonOutcome {
    if opted_out {
        DaemonOutcome::OptedOut
    } else if !macos {
        DaemonOutcome::Manual
    } else {
        install_daemon_agent(spec, uid, launchctl, pause)
    }
}

/// [`daemon_step`] against the real machine, returning the text to print.
fn daemon_step_live(db_path: &Path, projects_dir: &Path) -> String {
    let home = dirs::home_dir().unwrap_or_default();
    let exe = std::env::current_exe().unwrap_or_default();
    let engine = exe.canonicalize().unwrap_or(exe);
    let spec = DaemonAgentSpec {
        engine: engine.clone(),
        home: home.clone(),
        db_path: absolute(db_path),
        projects_dir: absolute(projects_dir),
        claude_dir: find_on_path("claude").and_then(|p| p.parent().map(Path::to_path_buf)),
        no_codex_import: crate::daemon::codex_import_disabled(),
    };
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    let outcome = daemon_step(
        &spec,
        daemon_opted_out(),
        cfg!(target_os = "macos"),
        uid,
        &mut real_launchctl,
        &std::thread::sleep,
    );
    let mut text = daemon_report(&outcome, &engine);
    if matches!(
        outcome,
        DaemonOutcome::Running { .. } | DaemonOutcome::KeptExisting
    ) {
        text.push_str(if wait_for_daemon(&home, Duration::from_secs(20)) {
            "  Daemon is running.\n"
        } else {
            "  launchd has the daemon, but it has not taken its lock yet (a large corpus loads slowly). Check: csr-engine status\n"
        });
    }
    text
}

fn absolute(path: &Path) -> std::path::PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The first executable called `name` on PATH.
fn find_on_path(name: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// The lines setup prints for each outcome (stderr, like every other step).
fn daemon_report(outcome: &DaemonOutcome, engine: &Path) -> String {
    let manual = format!(
        "  To run it by hand: {} daemon   (keep it running; it does the overnight dreams)\n",
        engine.display()
    );
    match outcome {
        DaemonOutcome::OptedOut => {
            "  Daemon skipped (CSR_NO_DAEMON is set). Dreams and background enrichment need it.\n"
                .to_owned()
                + &manual
        }
        DaemonOutcome::Manual => {
            "  No launchd on this system, so setup does not start the daemon.\n".to_owned() + &manual
        }
        DaemonOutcome::Running { rewritten } => format!(
            "  Daemon agent {} and started: {DAEMON_LABEL}\n  Stop it with: launchctl bootout gui/$(id -u)/{DAEMON_LABEL}\n",
            if *rewritten { "installed" } else { "already installed" },
        ),
        DaemonOutcome::KeptExisting => format!(
            "  Kept your existing {DAEMON_LABEL} agent and started it.\n"
        ),
        DaemonOutcome::Failed(why) => format!(
            "  The daemon did not start: {why}\n  Setup is otherwise complete. Retry with: csr-engine setup\n{manual}"
        ),
    }
}

/// After a successful load, wait briefly for the daemon to hold its lock, so the
/// summary can say whether it is up. A large corpus can take longer than the
/// wait; then it says so rather than claim success.
fn wait_for_daemon(home: &Path, wait: Duration) -> bool {
    let lock = home.join(".claude-self-reflect/daemon.lock");
    let deadline = Instant::now() + wait;
    loop {
        if crate::daemon::lock_file_held(&lock) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_install_failure_is_carried_not_dropped() {
        let carried = hook_install_step(|| Err(anyhow::anyhow!("settings.json is not valid JSON")));
        assert_eq!(carried.as_deref(), Some("settings.json is not valid JSON"));
        assert_eq!(hook_install_step(|| Ok(())), None);

        let report = incomplete_report(None, carried.as_deref());
        assert!(report.contains("Hook install failed: settings.json is not valid JSON"));
        assert!(report.contains("Hooks from an earlier install, if any, are unchanged."));
        assert!(!report.contains("NOT installed"), "{report}");
        assert!(!report.contains("inactive"), "{report}");
        assert!(report.contains("csr-engine hook install --apply"));
        assert!(report.contains("MCP registration are done"), "{report}");
        // the same non-zero status the MCP failure path returns
        assert_eq!(incomplete_error(false, true), "hook install failed");
        assert_eq!(incomplete_error(true, false), "MCP server not registered");
    }

    #[test]
    fn incomplete_report_keeps_the_mcp_wording_and_adds_the_hooks() {
        let mcp = mcp_registration_error("/bin/csr-engine", "boom", &PriorEntry::None);
        let only_mcp = incomplete_report(Some(&mcp), None);
        assert!(
            only_mcp.starts_with("  Hooks and import are done. The MCP server is NOT registered:")
        );
        assert!(!only_mcp.contains("Hook install failed"));

        let both = incomplete_report(Some(&mcp), Some("disk full"));
        assert!(both.contains("Hook install failed: disk full"));
        assert!(both.contains("  The import is done. The MCP server is NOT registered:"));
        assert!(!both.contains("Hooks and import are done"));
        assert_eq!(
            incomplete_error(true, true),
            "MCP server not registered and hook install failed"
        );
    }

    #[test]
    fn setup_next_steps_name_the_binarys_own_retry_command() {
        let text = next_steps(None);
        assert!(text.starts_with("Next steps:\n  1. Restart Claude Code"));
        assert!(text.ends_with("To install it, run: csr-engine companion plugin install\n"));
        // the steps name a recap and a plain-words question, not a tool or a key
        assert!(text.contains("its first line recaps where you left off"));
        assert!(text.contains("\"what did we work on last week?\""));
        assert!(!text.contains("reflect_on_past"), "{text}");
        assert!(!text.contains("--anthropic-key"), "{text}");
        assert!(!text.contains("csr-engine daemon"), "{text}");
        // no path ends with the GitHub marketplace's lines
        assert!(!text.contains("/plugin marketplace add"), "{text}");
        assert!(!text.contains("/plugin install"), "{text}");
    }

    #[test]
    fn test_save_anthropic_key_new_file() {
        let dir = tempfile::tempdir().unwrap();
        save_anthropic_key(dir.path(), "sk-ant-test123").unwrap();
        let content = std::fs::read_to_string(dir.path().join(".env")).unwrap();
        assert!(content.contains("ANTHROPIC_API_KEY=sk-ant-test123"));
    }

    #[test]
    fn test_save_anthropic_key_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join(".env");
        std::fs::write(&env_path, "OTHER_VAR=hello\nANTHROPIC_API_KEY=old\n").unwrap();

        save_anthropic_key(dir.path(), "sk-ant-new").unwrap();
        let content = std::fs::read_to_string(&env_path).unwrap();
        assert!(content.contains("ANTHROPIC_API_KEY=sk-ant-new"));
        assert!(content.contains("OTHER_VAR=hello"));
        assert!(!content.contains("old"));
    }

    #[test]
    fn test_count_total_files_nonexistent() {
        let count = count_total_files(Path::new("/tmp/nonexistent-csr-setup-test"));
        assert_eq!(count, 0);
    }

    // ── dreaming consent (locked decision 15) ──

    /// Seed one candidate episode: a v2 reflection with a partial outcome and
    /// at least one touched file — exactly what a night pass would consider.
    fn seed_candidate(storage: &Storage, session: &str) {
        let content = serde_json::json!({
            "schema": "v2",
            "session_id": session,
            "project": "proj",
            "timestamp": "2026-08-11T10:00:00Z",
            "request": "do the thing",
            "outcome": "partial",
            "completed": "half of it",
            "files_modified": ["src/a.rs"],
        })
        .to_string();
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO reflections (id, content, tags, timestamp)
                     VALUES (?1, ?2, '[]', '2026-08-11T10:00:00Z')",
                    rusqlite::params![session, content],
                )?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn the_estimate_is_computed_from_the_real_corpus_and_labelled_an_estimate() {
        let storage = Storage::open_memory().unwrap();
        let empty = night_estimate(&storage, EffortTier::Balanced);
        assert_eq!(empty.candidates, 0, "an empty corpus estimates from zero");
        assert_eq!(empty.total_tokens(), 0);

        for index in 0..3 {
            seed_candidate(&storage, &format!("session-{index}"));
        }
        let seeded = night_estimate(&storage, EffortTier::Balanced);
        assert_eq!(
            seeded.candidates, 3,
            "the estimate must count the corpus, not a constant"
        );
        assert!(seeded.total_tokens() > 0);
        let label = seeded.label(EffortTier::Balanced);
        assert!(label.contains("estimate"), "not labelled: {label}");
        assert!(label.contains("3 candidate episodes"), "no basis: {label}");
    }

    #[test]
    fn a_non_interactive_run_keeps_dreaming_on_and_records_the_decision() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        let storage = Storage::open_memory().unwrap();
        std::env::set_var("CSR_AUTO_SETUP", "1");
        let granted = dreaming_consent_step(&storage);
        std::env::remove_var("CSR_AUTO_SETUP");
        assert!(granted.unwrap(), "the default is ON");
        assert!(!dream_cadence::consent_declined(&storage));
        assert_eq!(
            storage.get_meta(dream_cadence::META_CONSENT).unwrap(),
            Some(dream_cadence::CONSENT_GRANTED.to_string()),
            "a taken default is still recorded, not left ambiguous"
        );
    }

    #[test]
    fn auto_setup_makes_the_run_non_interactive() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        std::env::set_var("CSR_AUTO_SETUP", "1");
        let auto = interactive();
        std::env::remove_var("CSR_AUTO_SETUP");
        assert!(!auto, "CSR_AUTO_SETUP=1 must never prompt");
    }

    #[test]
    fn a_declined_consent_stops_the_daemon_dreaming() {
        let storage = Storage::open_memory().unwrap();
        assert!(
            !dream_cadence::consent_declined(&storage),
            "never asked is not declined"
        );
        dream_cadence::record_consent(&storage, false).unwrap();
        assert!(dream_cadence::consent_declined(&storage));
        dream_cadence::record_consent(&storage, true).unwrap();
        assert!(!dream_cadence::consent_declined(&storage));
    }

    /// The exact stderr Claude Code 2.1.x emits when the name is taken. Only
    /// this case earns a remove-then-retry; everything else has to surface.
    #[test]
    fn test_classify_add_failure_already_exists() {
        assert_eq!(
            classify_add_failure("MCP server claude-self-reflect already exists in user config"),
            AddFailure::AlreadyRegistered
        );
        assert_eq!(
            classify_add_failure("  MCP server ALREADY EXISTS in user config\n"),
            AddFailure::AlreadyRegistered
        );
    }

    #[test]
    fn test_classify_add_failure_anything_else_is_fatal() {
        for stderr in [
            "",
            "error: unknown option '-s'",
            "EACCES: permission denied, open '/Users/me/.claude.json'",
            "Invalid transport type",
        ] {
            assert_eq!(
                classify_add_failure(stderr),
                AddFailure::Fatal,
                "{stderr:?} must not trigger a remove"
            );
        }
    }

    /// The error a user sees has to contain both commands, with the real
    /// binary path — it is the only way out when the retry also fails.
    #[test]
    fn test_mcp_registration_error_names_the_add_command() {
        let failure =
            mcp_registration_error("/home/me/.local/bin/csr-engine", "boom", &PriorEntry::None);
        assert!(
            failure.detail.contains(
                "claude mcp add claude-self-reflect /home/me/.local/bin/csr-engine -s user"
            ),
            "{}",
            failure.detail
        );
        // Nothing is registered, so a remove would only fail.
        assert!(
            !failure.detail.contains("claude mcp remove"),
            "{}",
            failure.detail
        );
        assert!(failure.detail.contains("boom"), "{}", failure.detail);
        assert!(
            failure.summary.contains("NOT registered"),
            "{}",
            failure.summary
        );
    }

    /// A path a shell would mangle has to be pasteable.
    #[test]
    fn test_mcp_registration_error_quotes_the_binary_path() {
        let failure =
            mcp_registration_error("/Users/me/CSR Tools/csr-engine", "boom", &PriorEntry::None);
        assert!(
            failure.detail.contains(
                "claude mcp add claude-self-reflect '/Users/me/CSR Tools/csr-engine' -s user"
            ),
            "{}",
            failure.detail
        );
    }

    /// An entry that is still there has to be removed first, or the add the
    /// user pastes fails with the same "already exists" we just hit.
    #[test]
    fn test_mcp_registration_error_for_a_surviving_entry_says_remove_first() {
        for prior in [
            PriorEntry::Intact(Some("/old/csr-engine".to_string())),
            PriorEntry::Restored {
                command: Some("/old/csr-engine".to_string()),
                exact: true,
            },
            PriorEntry::Occupied(Some("/other/thing".to_string())),
        ] {
            let failure = mcp_registration_error("/new/csr-engine", "boom", &prior);
            assert!(
                failure
                    .detail
                    .contains("claude mcp remove claude-self-reflect -s user"),
                "{prior:?}: {}",
                failure.detail
            );
            let remove_at = failure.detail.find("claude mcp remove").unwrap();
            let add_at = failure.detail.find("claude mcp add").unwrap();
            assert!(remove_at < add_at, "{prior:?}: remove must come first");
            assert!(
                !failure.summary.contains("NOT registered"),
                "{prior:?}: it IS registered, just not to the new binary"
            );
        }
    }

    /// We refused to remove because we could not snapshot it — the entry is
    /// still there, but we cannot name what it points at. An SSE or HTTP entry
    /// has no binary either, so the wording must not claim one.
    #[test]
    fn test_mcp_registration_error_for_an_unreadable_snapshot() {
        let failure =
            mcp_registration_error("/new/csr-engine", "unreadable", &PriorEntry::Intact(None));
        assert!(
            failure
                .detail
                .contains("claude mcp remove claude-self-reflect -s user"),
            "{}",
            failure.detail
        );
        assert!(
            failure
                .summary
                .contains("still registered to the previous MCP entry, not the new one"),
            "{}",
            failure.summary
        );
        assert!(
            !failure.summary.contains("binary"),
            "an unknown entry may not point at a binary at all: {}",
            failure.summary
        );
    }

    /// Something else took the name while we were working. An entry is present,
    /// so add-only advice would fail — and it is not the one we captured.
    #[test]
    fn test_mcp_registration_error_when_the_name_was_taken_by_someone_else() {
        let failure = mcp_registration_error(
            "/new/csr-engine",
            "MCP server claude-self-reflect already exists in user config",
            &PriorEntry::Occupied(Some("/other/thing".to_string())),
        );
        assert!(
            failure
                .detail
                .contains("something else registered the name (/other/thing)"),
            "{}",
            failure.detail
        );
        assert!(
            failure
                .summary
                .contains("name is held by another registration (/other/thing)"),
            "{}",
            failure.summary
        );
        assert!(
            failure
                .detail
                .contains("claude mcp remove claude-self-reflect -s user"),
            "{}",
            failure.detail
        );
    }

    /// Removing a working registration and failing to re-add it is the worst
    /// outcome available, so it has to read differently from the others — and
    /// it has to hand back the snapshot, which is now the only copy.
    #[test]
    fn test_mcp_registration_error_for_a_lost_entry() {
        let failure = mcp_registration_error(
            "/new/csr-engine",
            "boom",
            &PriorEntry::Lost {
                command: Some("/old/csr-engine".to_string()),
                snapshot: r#"{"command":"/old/csr-engine","env":{"A":"b"}}"#.to_string(),
            },
        );
        assert!(
            failure.detail.contains("could not be restored"),
            "{}",
            failure.detail
        );
        assert!(
            failure.detail.contains("Nothing is registered right now"),
            "{}",
            failure.detail
        );
        // The snapshot is pasteable: quoted, because JSON is full of characters
        // a shell would eat.
        assert!(
            failure.detail.contains(
                r#"claude mcp add-json claude-self-reflect '{"command":"/old/csr-engine","env":{"A":"b"}}' -s user"#
            ),
            "{}",
            failure.detail
        );
        // Nothing is there, so a plain remove would only fail.
        assert!(
            !failure
                .detail
                .contains("claude mcp remove claude-self-reflect -s user"),
            "{}",
            failure.detail
        );
        assert!(
            failure.summary.contains("NOT registered"),
            "{}",
            failure.summary
        );
    }

    /// A partial restore has to admit what it dropped.
    #[test]
    fn test_mcp_registration_error_for_a_partial_restore() {
        let failure = mcp_registration_error(
            "/new/csr-engine",
            "boom",
            &PriorEntry::Restored {
                command: Some("/old/csr-engine".to_string()),
                exact: false,
            },
        );
        assert!(
            failure.detail.contains("env vars it had were not restored"),
            "{}",
            failure.detail
        );
    }

    #[test]
    fn test_still_registered_is_true_only_while_an_entry_survives() {
        assert!(!PriorEntry::None.still_registered());
        assert!(!PriorEntry::Lost {
            command: Some("/old".to_string()),
            snapshot: "{}".to_string()
        }
        .still_registered());
        assert!(PriorEntry::Intact(None).still_registered());
        assert!(PriorEntry::Intact(Some("/old".to_string())).still_registered());
        assert!(PriorEntry::Occupied(None).still_registered());
        assert!(PriorEntry::Restored {
            command: Some("/old".to_string()),
            exact: true
        }
        .still_registered());
    }

    /// Claude Code's own help requires `--` before a subprocess command, so an
    /// ordinary `args: ["--serve"]` must not be handed to its option parser.
    #[test]
    fn test_legacy_restore_args_protects_leading_dash_arguments() {
        let entry = serde_json::json!({
            "type": "stdio",
            "command": "/old/csr-engine",
            "args": ["--serve", "--limit", "5"],
            "env": {"CSR_NO_DREAMING": "1"}
        });
        assert_eq!(
            legacy_restore_args(&entry),
            Some(vec![
                "mcp".to_string(),
                "add".to_string(),
                "-s".to_string(),
                "user".to_string(),
                "claude-self-reflect".to_string(),
                "--".to_string(),
                "/old/csr-engine".to_string(),
                "--serve".to_string(),
                "--limit".to_string(),
                "5".to_string(),
            ])
        );
        let separator = legacy_restore_args(&entry)
            .unwrap()
            .iter()
            .position(|a| a == "--")
            .unwrap();
        let command = legacy_restore_args(&entry)
            .unwrap()
            .iter()
            .position(|a| a == "/old/csr-engine")
            .unwrap();
        assert!(separator < command, "-- must precede the command");
    }

    /// A `type` this form cannot express, or a missing command, means no
    /// fallback at all — rather than inventing an executable.
    #[test]
    fn test_legacy_restore_args_refuses_what_it_cannot_express() {
        for entry in [
            serde_json::json!({"type": "sse", "url": "https://example.test/mcp"}),
            serde_json::json!({"type": "http", "url": "https://example.test/mcp"}),
            serde_json::json!({"type": "stdio"}),
            serde_json::json!({"type": "stdio", "command": 7}),
            serde_json::json!({"type": "stdio", "command": "/old/csr-engine", "args": [7]}),
            // Present but malformed fields are not "absent": a hand-edited
            // `"args": "--serve"` must not be restored as a bare command.
            serde_json::json!({"type": "stdio", "command": "/old/csr-engine", "args": "--serve"}),
            serde_json::json!({"type": "stdio", "command": "/old/csr-engine", "args": {"a": 1}}),
            serde_json::json!({"type": 5, "command": "/old/csr-engine"}),
            serde_json::json!({"type": null, "command": "/old/csr-engine"}),
        ] {
            assert_eq!(legacy_restore_args(&entry), None, "{entry}");
        }
        // No `type` at all is the historical stdio shape and still qualifies.
        assert!(legacy_restore_args(&serde_json::json!({"command": "/old/csr-engine"})).is_some());
    }

    /// A non-stdio snapshot that add-json could not replay has to come back as
    /// Lost with its JSON, not as a bogus partial restore.
    #[test]
    fn test_mcp_registration_error_for_a_lost_non_stdio_entry_prints_its_json() {
        let snapshot = r#"{"type":"sse","url":"https://example.test/mcp"}"#;
        let failure = mcp_registration_error(
            "/new/csr-engine",
            "boom",
            &PriorEntry::Lost {
                command: None,
                snapshot: snapshot.to_string(),
            },
        );
        assert!(
            failure
                .detail
                .contains("The previous registration was removed and could not be restored"),
            "{}",
            failure.detail
        );
        assert!(failure.detail.contains(snapshot), "{}", failure.detail);
        assert!(
            !failure.detail.contains("unknown"),
            "never synthesise a command name: {}",
            failure.detail
        );
    }

    /// The whole object is what gets replayed, so `args` and `env` have to
    /// survive the capture.
    #[test]
    fn test_entry_command_reads_the_command_out_of_a_whole_entry() {
        let entry = serde_json::json!({
            "type": "stdio",
            "command": "/old/csr-engine",
            "args": ["--serve"],
            "env": {"CSR_NO_DREAMING": "1"}
        });
        assert_eq!(entry_command(&entry), Some("/old/csr-engine".to_string()));
        assert_eq!(entry_command(&serde_json::json!({})), None);
    }

    // ── the relay plugin (fake `claude` with a little state; a temp HOME) ──

    const ENGINE: &str = "/opt/csr/bin/csr-engine";

    /// A fake `claude plugin` that keeps the marketplace listing and the plugin's state in
    /// files next to it. Every call appends its argv to `argv.log`.
    struct RelayFake {
        dir: tempfile::TempDir,
        home: tempfile::TempDir,
        bin: std::path::PathBuf,
    }

    impl RelayFake {
        /// `market` is what `marketplace list --json` prints; `plugin` is `absent`, `stale`
        /// (an older version, cached text that is not the relay) or `current`.
        fn new(market: &str, plugin: &str, update_refreshes: bool, refuses_add: bool) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let home = tempfile::tempdir().unwrap();
            let d = dir.path();
            let built = emit::write_marketplace(home.path(), Some(Path::new(ENGINE))).unwrap();
            let fresh = d.join("cache-current");
            std::fs::create_dir_all(fresh.join("hooks")).unwrap();
            std::fs::write(fresh.join("hooks/register.js"), &built.relay).unwrap();
            let old = d.join("cache-stale");
            std::fs::create_dir_all(old.join("hooks")).unwrap();
            std::fs::write(old.join("hooks/register.js"), "// an older relay\n").unwrap();
            std::fs::write(
                d.join("market"),
                market.replace("DIR", built.dir.to_str().unwrap()),
            )
            .unwrap();
            std::fs::write(d.join("plugin"), plugin).unwrap();
            std::fs::write(d.join("update_refreshes"), update_refreshes.to_string()).unwrap();
            std::fs::write(d.join("refuses_add"), refuses_add.to_string()).unwrap();
            std::fs::write(d.join("enabled"), "true").unwrap();
            let id = "csr-companion@claude-self-reflect";
            let version = env!("CARGO_PKG_VERSION");
            let script = format!(
                r#"#!/bin/sh
D='{d}'
echo "$@" >> "$D/argv.log"
case "$2" in
  marketplace)
    case "$3" in
      list) cat "$D/market" ;;
      add)
        if grep -q github "$D/market" && [ "$(cat "$D/refuses_add")" = true ]; then
          echo "already exists" >&2; exit 1
        fi
        if [ -f "$D/add_fails_once" ] && ! grep -q github "$D/market"; then
          rm "$D/add_fails_once"; echo "network down" >&2; exit 1
        fi ;;
      remove) echo '[]' > "$D/market"; echo absent > "$D/plugin" ;;
    esac ;;
  list)
    E=$(cat "$D/enabled")
    case "$(cat "$D/plugin")" in
      absent) echo '[]' ;;
      stale) echo '[{{"id":"{id}","version":"0.5.0","scope":"user","enabled":'$E',"installPath":"{d}/cache-stale"}}]' ;;
      current) echo '[{{"id":"{id}","version":"{version}","scope":"user","enabled":'$E',"installPath":"{d}/cache-current"}}]' ;;
    esac ;;
  disable)
    if [ -f "$D/disable_fails" ]; then echo "settings are read-only" >&2; exit 1; fi
    if [ -f "$D/disable_fails_once" ]; then rm "$D/disable_fails_once"; echo "settings are read-only" >&2; exit 1; fi
    echo false > "$D/enabled"
    echo '{{"command":"disable","outcome":"ok","plugin":"{id}","scope":"user","message":"ok"}}' ;;
  install)
    if [ -f "$D/install_fails_once" ]; then rm "$D/install_fails_once"; echo "disk full" >&2; exit 1; fi
    echo current > "$D/plugin"; echo true > "$D/enabled"
    echo '{{"command":"install","outcome":"ok","plugin":"{id}","scope":"user","message":"ok"}}' ;;
  update)
    if [ "$(cat "$D/update_refreshes")" = true ]; then echo current > "$D/plugin"; fi
    echo '{{"command":"update","outcome":"ok","plugin":"{id}","scope":"user","message":"ok"}}' ;;
  uninstall) echo absent > "$D/plugin"
    echo '{{"command":"uninstall","outcome":"ok","plugin":"{id}","scope":"user","message":"ok"}}' ;;
  configure)
    if [ -f "$D/configure_fails" ]; then echo "keychain locked" >&2; exit 1; fi
    if [ -f "$D/configure_silent" ]; then exit 0; fi
    if [ -f "$D/configure_none" ]; then echo '{{"configured":[],"inputs":{{}}}}'; exit 0; fi
    echo '{{"configured":["dreams"],"inputs":{{"dreams":"false","autoCompact":"suggest"}}}}' ;;
esac
"#,
                d = d.display()
            );
            let bin = d.join("claude");
            std::fs::write(&bin, script).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            RelayFake { dir, home, bin }
        }

        /// The user had switched the plugin off.
        fn disabled(self) -> Self {
            std::fs::write(self.dir.path().join("enabled"), "false").unwrap();
            self
        }

        /// Make the fake's `configure` (the option read) behave in a way: `configure_fails`
        /// (exit 1), `configure_silent` (exit 0, no output) or `configure_none` (no options).
        fn with(self, flag: &str) -> Self {
            std::fs::write(self.dir.path().join(flag), "").unwrap();
            self
        }

        /// Where a half-finished migration keeps the user's options.
        fn pending(&self) -> std::path::PathBuf {
            self.home
                .path()
                .join(".claude-self-reflect/csr-companion/plugin-migration.json")
        }

        fn enabled(&self) -> String {
            std::fs::read_to_string(self.dir.path().join("enabled")).unwrap()
        }

        fn run(&self) -> RelayReport {
            install_relay(
                &self.bin,
                self.home.path(),
                Path::new(ENGINE),
                Duration::from_secs(10),
            )
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("argv.log"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    const NONE_REGISTERED: &str = "[]";
    const HERE: &str = r#"[{"name":"claude-self-reflect","source":"directory","path":"DIR"}]"#;
    const ON_GITHUB: &str = r#"[{"name":"claude-self-reflect","source":"github","repo":"ramakay/claude-self-reflect"}]"#;

    #[test]
    fn market_state_reads_the_listing() {
        let dir = Path::new("/private/tmp/csr-checkout");
        let json = |text: &str| serde_json::from_str::<serde_json::Value>(text).unwrap();
        assert_eq!(market_state(&json("[]"), dir), MarketState::Absent);
        assert_eq!(
            market_state(&json(r#"[{"name":"other","source":"github"}]"#), dir),
            MarketState::Absent
        );
        assert_eq!(
            market_state(
                &json(
                    r#"[{"name":"claude-self-reflect","source":"directory","path":"/private/tmp/csr-checkout"}]"#
                ),
                dir
            ),
            MarketState::Here
        );
        assert_eq!(
            market_state(&json(ON_GITHUB), dir),
            MarketState::Elsewhere("github ramakay/claude-self-reflect".into())
        );
        assert!(matches!(
            market_state(
                &json(
                    r#"[{"name":"claude-self-reflect","source":"directory","path":"/elsewhere"}]"#
                ),
                dir
            ),
            MarketState::Elsewhere(_)
        ));
    }

    #[test]
    fn relay_install_on_a_fresh_machine_adds_the_marketplace_and_installs() {
        let fake = RelayFake::new(NONE_REGISTERED, "absent", true, false);
        let report = fake.run();
        let CompanionOutcome::Installed(info) = &report.outcome else {
            panic!("{report:?}");
        };
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert!(report.changed);
        let dir = emit::marketplace_dir(fake.home.path());
        let calls = fake.calls();
        assert_eq!(calls[0], "plugin marketplace list --json");
        assert_eq!(
            calls[1],
            format!("plugin marketplace add {}", dir.display())
        );
        assert_eq!(calls[2], "plugin list --json");
        assert_eq!(
            calls[3],
            "plugin install csr-companion@claude-self-reflect --json"
        );
        assert!(!calls
            .iter()
            .any(|c| c.contains("remove") || c.contains("update")));
        let text = next_steps(Some(&report));
        assert!(text.contains(RELOAD_LINE), "{text}");
    }

    #[test]
    fn relay_install_changes_nothing_when_it_is_current() {
        let fake = RelayFake::new(HERE, "current", true, false);
        let report = fake.run();
        assert!(
            matches!(report.outcome, CompanionOutcome::AlreadyInstalled(_)),
            "{report:?}"
        );
        assert!(!report.changed);
        let calls = fake.calls();
        assert!(
            calls.iter().all(|c| c.ends_with("list --json")),
            "something other than a listing ran: {calls:?}"
        );
        assert!(!next_steps(Some(&report)).contains("/reload-plugins"));
    }

    #[test]
    fn relay_install_moves_a_github_marketplace_and_updates_the_plugin() {
        let fake = RelayFake::new(ON_GITHUB, "stale", true, false);
        let report = fake.run();
        assert!(
            matches!(report.outcome, CompanionOutcome::Installed(_)),
            "{report:?}"
        );
        assert!(report.changed);
        let calls = fake.calls();
        assert!(calls
            .iter()
            .any(|c| c.starts_with("plugin marketplace add ")));
        assert!(!calls.iter().any(|c| c.contains("marketplace remove")));
        assert!(calls.contains(&"plugin update csr-companion@claude-self-reflect --json".into()));
        assert!(!calls.iter().any(|c| c.contains("uninstall")));
        assert!(report
            .steps
            .iter()
            .any(|s| s.starts_with("marketplace moved here (was: github")));
        assert!(report
            .steps
            .iter()
            .any(|s| s.starts_with("plugin updated: 0.5.0 -> ")));
    }

    #[test]
    fn relay_install_reinstalls_when_the_update_keeps_the_old_cache() {
        let fake = RelayFake::new(HERE, "stale", false, false);
        let report = fake.run();
        assert!(
            matches!(report.outcome, CompanionOutcome::Installed(_)),
            "{report:?}"
        );
        let calls = fake.calls();
        let at = |needle: &str| calls.iter().position(|c| c.contains(needle)).unwrap();
        assert!(at("plugin update") < at("plugin uninstall"));
        assert!(calls.iter().any(|c| c.ends_with("--keep-data --json")));
        assert!(at("plugin uninstall") < at("plugin install"));
        // the option the user had set goes back in at install
        assert!(calls
            .iter()
            .any(|c| c.starts_with("plugin install ") && c.ends_with("--config dreams=false")));
        assert!(report
            .steps
            .iter()
            .any(|s| s.starts_with("plugin reinstalled")));
    }

    #[test]
    fn a_reinstall_keeps_a_plugin_the_user_had_disabled_disabled() {
        let fake = RelayFake::new(HERE, "stale", false, false).disabled();
        let report = fake.run();
        assert!(
            matches!(report.outcome, CompanionOutcome::Installed(_)),
            "{report:?}"
        );
        let calls = fake.calls();
        let at = |needle: &str| calls.iter().position(|c| c.contains(needle)).unwrap();
        assert!(at("plugin install") < at("plugin disable"));
        assert!(calls.contains(&"plugin disable csr-companion@claude-self-reflect --json".into()));
        assert_eq!(fake.enabled().trim(), "false");
        // an enabled plugin is never disabled
        let on = RelayFake::new(HERE, "stale", false, false);
        on.run();
        assert!(!on.calls().iter().any(|c| c.contains("plugin disable")));
        assert_eq!(on.enabled().trim(), "true");
    }

    #[test]
    fn a_removed_marketplace_also_keeps_a_disabled_plugin_disabled() {
        let fake = RelayFake::new(ON_GITHUB, "stale", true, true).disabled();
        let report = fake.run();
        assert!(
            matches!(report.outcome, CompanionOutcome::Installed(_)),
            "{report:?}"
        );
        let calls = fake.calls();
        let at = |needle: &str| calls.iter().position(|c| c.contains(needle)).unwrap();
        assert!(at("marketplace remove") < at("plugin install"));
        assert!(at("plugin install") < at("plugin disable"));
        assert_eq!(fake.enabled().trim(), "false");
    }

    #[test]
    fn a_failed_disable_is_reported_in_setups_style() {
        let fake = RelayFake::new(HERE, "stale", false, false).disabled();
        std::fs::write(fake.dir.path().join("disable_fails"), "").unwrap();
        let report = fake.run();
        let CompanionOutcome::Failed { reason } = &report.outcome else {
            panic!("{report:?}");
        };
        assert!(
            reason.contains("`claude plugin disable` failed"),
            "{reason}"
        );
        assert!(reason.contains("settings are read-only"), "{reason}");
        assert!(reason.contains("claude plugin disable csr-companion@claude-self-reflect"));
        let text = next_steps(Some(&report));
        assert!(text.contains("companion: not installed:"), "{text}");
    }

    #[test]
    fn relay_install_removes_and_adds_when_add_over_the_name_is_refused() {
        let fake = RelayFake::new(ON_GITHUB, "stale", true, true);
        let report = fake.run();
        assert!(
            matches!(report.outcome, CompanionOutcome::Installed(_)),
            "{report:?}"
        );
        let calls = fake.calls();
        let at = |needle: &str| calls.iter().position(|c| c.contains(needle)).unwrap();
        assert!(at("plugin configure") < at("marketplace remove"));
        assert!(at("marketplace remove") < at("plugin install"));
        assert!(calls
            .iter()
            .any(|c| c.starts_with("plugin install ") && c.ends_with("--config dreams=false")));
        // a run that finishes leaves nothing behind
        assert!(!fake.pending().exists());
    }

    #[test]
    fn an_absent_plugin_has_no_options_to_read_before_the_marketplace_moves() {
        // `configure` would fail for a plugin that is not installed; nothing to lose then
        let fake = RelayFake::new(ON_GITHUB, "absent", true, true).with("configure_fails");
        let report = fake.run();
        assert!(
            matches!(report.outcome, CompanionOutcome::Installed(_)),
            "{report:?}"
        );
        let calls = fake.calls();
        assert!(!calls.iter().any(|c| c.contains("plugin configure")));
        assert!(calls.iter().any(|c| c.contains("marketplace remove")));
        assert!(!calls.iter().any(|c| c.contains("--config")));
    }

    /// S3: a failed read of the options stops the step that would delete them.
    /// `updated`: the step before it (`plugin update`) already changed the plugin.
    fn failed_option_read_stops(fake: &RelayFake, what: &str, updated: bool) {
        let report = fake.run();
        let CompanionOutcome::Failed { reason } = &report.outcome else {
            panic!("{what}: {report:?}");
        };
        assert!(
            reason.contains("csr-engine companion plugin install"),
            "{what}: {reason}"
        );
        assert!(reason.contains("options"), "{what}: {reason}");
        assert_eq!(report.changed, updated, "{what}");
        assert_eq!(
            next_steps(Some(&report)).contains(RELOAD_LINE),
            updated,
            "{what}"
        );
        let calls = fake.calls();
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("marketplace remove") || c.contains("plugin uninstall")),
            "{what}: a destructive step ran: {calls:?}"
        );
        assert!(!calls.iter().any(|c| c.starts_with("plugin install ")));
        assert!(!fake.pending().exists(), "{what}");
    }

    #[test]
    fn a_failed_option_read_stops_the_marketplace_move() {
        for flag in ["configure_fails", "configure_silent"] {
            let fake = RelayFake::new(ON_GITHUB, "stale", true, true).with(flag);
            failed_option_read_stops(&fake, flag, false);
        }
    }

    #[test]
    fn a_failed_option_read_stops_the_cache_repair() {
        for flag in ["configure_fails", "configure_silent"] {
            let fake = RelayFake::new(HERE, "stale", false, false).with(flag);
            failed_option_read_stops(&fake, flag, true);
        }
    }

    #[test]
    fn an_empty_but_successful_option_read_goes_on_with_no_options() {
        let fake = RelayFake::new(ON_GITHUB, "stale", true, true).with("configure_none");
        let report = fake.run();
        assert!(
            matches!(report.outcome, CompanionOutcome::Installed(_)),
            "{report:?}"
        );
        let calls = fake.calls();
        assert!(calls.iter().any(|c| c.contains("marketplace remove")));
        assert!(!calls.iter().any(|c| c.contains("--config")));
    }

    /// S4: the marketplace is removed, the add fails, and the options survive for the rerun.
    #[test]
    fn a_migration_that_fails_half_way_keeps_the_options_for_the_rerun() {
        let fake = RelayFake::new(ON_GITHUB, "stale", true, true)
            .disabled()
            .with("add_fails_once");
        let first = fake.run();
        let CompanionOutcome::Failed { reason } = &first.outcome else {
            panic!("{first:?}");
        };
        assert!(reason.contains("network down"), "{reason}");
        // a plugin was uninstalled: the failure's report still says to reload
        assert!(first.changed);
        assert!(next_steps(Some(&first)).contains(RELOAD_LINE));
        assert!(fake.pending().is_file(), "the options were not saved");
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(fake.pending())
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode, 0o600);

        // the plugin is gone now; the rerun installs with what the user had
        let before = fake.calls().len();
        let second = fake.run();
        assert!(
            matches!(second.outcome, CompanionOutcome::Installed(_)),
            "{second:?}"
        );
        let calls = fake.calls()[before..].to_vec();
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("plugin install ") && c.ends_with("--config dreams=false")),
            "{calls:?}"
        );
        assert!(calls.contains(&"plugin disable csr-companion@claude-self-reflect --json".into()));
        assert_eq!(fake.enabled().trim(), "false");
        assert!(
            !fake.pending().exists(),
            "the file outlived a confirmed install"
        );
    }

    #[test]
    fn a_failed_cache_repair_install_keeps_the_options_for_the_rerun() {
        let fake = RelayFake::new(HERE, "stale", false, false)
            .disabled()
            .with("install_fails_once");
        let first = fake.run();
        let CompanionOutcome::Failed { reason } = &first.outcome else {
            panic!("{first:?}");
        };
        assert!(reason.contains("disk full"), "{reason}");
        assert!(first.changed);
        assert!(next_steps(Some(&first)).contains(RELOAD_LINE));
        assert!(fake.pending().is_file(), "the options were not saved");

        let before = fake.calls().len();
        let second = fake.run();
        assert!(
            matches!(second.outcome, CompanionOutcome::Installed(_)),
            "{second:?}"
        );
        let calls = fake.calls()[before..].to_vec();
        assert!(calls
            .iter()
            .any(|c| c.starts_with("plugin install ") && c.ends_with("--config dreams=false")));
        assert_eq!(fake.enabled().trim(), "false");
        assert!(!fake.pending().exists());
    }

    /// A migration of a switched-off plugin whose install worked but whose disable failed:
    /// the rerun finds the plugin current and enabled, and the saved "off" still holds.
    fn failed_disable_after_a_migration() -> RelayFake {
        let fake = RelayFake::new(ON_GITHUB, "stale", true, true)
            .disabled()
            .with("disable_fails_once");
        let first = fake.run();
        let CompanionOutcome::Failed { reason } = &first.outcome else {
            panic!("{first:?}");
        };
        assert!(reason.contains("settings are read-only"), "{reason}");
        assert!(fake.pending().is_file(), "the saved state was dropped");
        assert_eq!(
            fake.enabled().trim(),
            "true",
            "the failed disable changed it"
        );
        fake
    }

    #[test]
    fn a_rerun_disables_a_current_plugin_the_user_had_switched_off() {
        let fake = failed_disable_after_a_migration();
        let before = fake.calls().len();
        let second = fake.run();
        assert!(
            matches!(second.outcome, CompanionOutcome::Installed(_)),
            "{second:?}"
        );
        assert!(second.changed);
        let calls = fake.calls()[before..].to_vec();
        assert!(
            calls.contains(&"plugin disable csr-companion@claude-self-reflect --json".into()),
            "{calls:?}"
        );
        assert!(!calls.iter().any(|c| c.starts_with("plugin install ")));
        assert_eq!(fake.enabled().trim(), "false");
        assert!(!fake.pending().exists(), "the file outlived the disable");
        // a third run has nothing left to do
        let before = fake.calls().len();
        let third = fake.run();
        assert!(
            matches!(third.outcome, CompanionOutcome::AlreadyInstalled(_)),
            "{third:?}"
        );
        assert!(!fake.calls()[before..].iter().any(|c| c.contains("disable")));
    }

    #[test]
    fn a_rerun_whose_disable_fails_again_keeps_the_file() {
        let fake = failed_disable_after_a_migration();
        std::fs::write(fake.dir.path().join("disable_fails"), "").unwrap();
        let second = fake.run();
        let CompanionOutcome::Failed { reason } = &second.outcome else {
            panic!("{second:?}");
        };
        assert!(reason.contains("settings are read-only"), "{reason}");
        assert!(fake.pending().is_file(), "the saved state was dropped");
        assert_eq!(fake.enabled().trim(), "true");
    }

    #[test]
    fn a_plugin_that_was_on_is_not_disabled_by_a_stale_file() {
        let fake = RelayFake::new(HERE, "current", true, false);
        let path = fake.pending();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"options":[],"enabled":true}"#).unwrap();
        let report = fake.run();
        assert!(
            matches!(report.outcome, CompanionOutcome::AlreadyInstalled(_)),
            "{report:?}"
        );
        assert!(!fake.calls().iter().any(|c| c.contains("disable")));
        assert!(!path.exists());
    }

    #[test]
    fn an_unreadable_snapshot_stops_setup_before_it_installs() {
        for (text, why) in [
            ("{ not json", "not JSON"),
            (
                r#"{"options":"dreams=false","enabled":false}"#,
                "options not a list",
            ),
            (
                r#"{"options":[["dreams"]],"enabled":false}"#,
                "pair without a value",
            ),
            (r#"{"options":[],"enabled":"no"}"#, "enabled not a bool"),
        ] {
            let fake = RelayFake::new(NONE_REGISTERED, "absent", true, false);
            let path = fake.pending();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            let report = fake.run();
            let CompanionOutcome::Failed { reason } = &report.outcome else {
                panic!("{why}: {report:?}");
            };
            assert!(
                reason.contains(&path.display().to_string()),
                "{why}: {reason}"
            );
            assert!(
                reason.contains("fix the file or delete it"),
                "{why}: {reason}"
            );
            let calls = fake.calls();
            assert!(
                !calls.iter().any(|c| c.starts_with("plugin install ")),
                "{why}: installed over an unreadable snapshot: {calls:?}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "{why}");
        }
    }

    #[test]
    fn a_snapshot_that_cannot_be_opened_is_an_error_not_none() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        assert_eq!(load_snapshot(home.path()), Ok(None));
        let path = pending_snapshot_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"options":[["a","b"]],"enabled":false}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // root reads anything; then there is nothing to show
        if std::fs::read_to_string(&path).is_err() {
            let err = load_snapshot(home.path()).unwrap_err();
            assert!(err.contains(&path.display().to_string()), "{err}");
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let snap = load_snapshot(home.path()).unwrap().unwrap();
        assert_eq!(snap.options, vec![("a".to_string(), "b".to_string())]);
        assert!(!snap.enabled);
    }

    #[test]
    fn a_normal_run_leaves_no_migration_file_behind() {
        for fake in [
            RelayFake::new(NONE_REGISTERED, "absent", true, false),
            RelayFake::new(ON_GITHUB, "stale", true, false),
            RelayFake::new(HERE, "stale", false, false),
            RelayFake::new(HERE, "current", true, false),
        ] {
            let report = fake.run();
            assert!(
                !matches!(report.outcome, CompanionOutcome::Failed { .. }),
                "{report:?}"
            );
            assert!(!fake.pending().exists());
        }
    }

    #[test]
    fn relay_install_reports_a_failed_step_and_stops() {
        let fake = RelayFake::new(ON_GITHUB, "absent", true, true);
        // `remove` also fails: make the fake's market file unwritable by replacing it with a dir
        let market = fake.dir.path().join("market");
        std::fs::remove_file(&market).unwrap();
        std::fs::create_dir(&market).unwrap();
        let report = fake.run();
        let CompanionOutcome::Failed { reason } = &report.outcome else {
            panic!("{report:?}");
        };
        assert!(reason.contains("marketplace list"), "{reason}");
        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        let text = next_steps(Some(&report));
        assert!(text.contains("companion: not installed:"), "{text}");
        assert!(
            text.contains("csr-engine companion plugin install"),
            "{text}"
        );
    }

    #[test]
    fn relay_install_with_no_claude_on_hand_fails_with_one_line() {
        let home = tempfile::tempdir().unwrap();
        let report = install_relay(
            Path::new("/nonexistent/claude"),
            home.path(),
            Path::new(ENGINE),
            Duration::from_secs(5),
        );
        let CompanionOutcome::Failed { reason } = &report.outcome else {
            panic!("{report:?}");
        };
        assert!(reason.contains("could not run"), "{reason}");
        assert!(!report.changed);
    }

    #[test]
    fn a_hanging_claude_is_stopped_at_the_timeout_and_only_that_child() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let bin = dir.path().join("claude");
        let pid = dir.path().join("pid");
        // `exec sleep` keeps the recorded pid equal to the process we spawned. The timeout
        // leaves a fresh script time to start and write its pid on a loaded machine.
        std::fs::write(
            &bin,
            format!("#!/bin/sh\necho $$ > '{}'\nexec sleep 30\n", pid.display()),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = Instant::now();
        let report = install_relay(&bin, home.path(), Path::new(ENGINE), Duration::from_secs(3));
        let waited = started.elapsed();
        assert!(
            waited < Duration::from_secs(10),
            "did not stop at the timeout: {waited:?}"
        );
        let CompanionOutcome::Failed { reason } = &report.outcome else {
            panic!("{report:?}");
        };
        assert!(reason.contains("did not finish within"), "{reason}");
        assert_eq!(reason.lines().count(), 1, "one line: {reason}");
        let text = next_steps(Some(&report));
        assert!(text.contains(&format!("companion: not installed: {reason}\n")));
        assert!(
            text.ends_with("csr-engine companion plugin install\n"),
            "{text}"
        );
        // On a loaded machine the limit can pass before the shell runs its first line:
        // then the kill hit the shell itself, and `wait` reaped it.
        let pid: i32 = match std::fs::read_to_string(&pid) {
            Ok(s) => s.trim().parse().unwrap(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => panic!("{e}"),
        };
        // SAFETY: signal 0 only probes for the pid's existence.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "child {pid} is still running after the timeout");
    }

    #[test]
    fn no_companion_writes_nothing_and_calls_nothing() {
        let fake = RelayFake::new(NONE_REGISTERED, "absent", true, false);
        let before = std::fs::read_dir(fake.home.path()).unwrap().count();
        let report = companion_step(
            false,
            &fake.bin,
            fake.home.path(),
            Path::new(ENGINE),
            Duration::from_secs(5),
        );
        assert_eq!(report, None);
        assert!(fake.calls().is_empty());
        assert_eq!(std::fs::read_dir(fake.home.path()).unwrap().count(), before);
        assert!(next_steps(None).contains("companion: skipped (--no-companion)"));
    }

    // ----- daemon agent -----

    fn agent_spec(home: &Path) -> DaemonAgentSpec {
        DaemonAgentSpec {
            engine: "/opt/csr/bin/csr-engine".into(),
            home: home.to_path_buf(),
            db_path: home.join(".claude-self-reflect/csr-engine.db"),
            projects_dir: home.join(".claude/projects"),
            claude_dir: Some("/opt/claude/bin".into()),
            no_codex_import: false,
        }
    }

    /// A launchctl stand-in: `loaded` answers `print`, `bootstrap_fails` the
    /// first N bootstraps. Records every call.
    struct FakeLaunchctl {
        loaded: bool,
        bootstrap_fails: usize,
        calls: Vec<String>,
    }

    impl FakeLaunchctl {
        fn new(loaded: bool) -> Self {
            Self {
                loaded,
                bootstrap_fails: 0,
                calls: Vec::new(),
            }
        }
        fn run(&mut self, args: &[&str]) -> std::result::Result<(), String> {
            self.calls.push(args[..args.len().min(2)].join(" "));
            match args[0] {
                "print" if self.loaded => Ok(()),
                "print" => Err("not loaded".into()),
                "bootstrap" if self.bootstrap_fails > 0 => {
                    self.bootstrap_fails -= 1;
                    Err("Bootstrap failed: 5: Input/output error".into())
                }
                "bootstrap" => {
                    self.loaded = true;
                    Ok(())
                }
                "bootout" => {
                    self.loaded = false;
                    Ok(())
                }
                _ => Ok(()),
            }
        }
    }

    fn install(spec: &DaemonAgentSpec, fake: &mut FakeLaunchctl) -> DaemonOutcome {
        install_daemon_agent(spec, 501, &mut |args| fake.run(args), &|_| {})
    }

    #[test]
    fn the_daemon_plist_uses_absolute_paths_and_the_real_binary() {
        let spec = agent_spec(Path::new("/Users/someone"));
        let text = render_daemon_plist(&spec);
        assert!(
            text.contains("<string>/opt/csr/bin/csr-engine</string>"),
            "{text}"
        );
        assert!(text.contains("<string>--db-path</string>"), "{text}");
        assert!(
            text.contains("<string>/Users/someone/.claude-self-reflect/csr-engine.db</string>"),
            "{text}"
        );
        assert!(
            text.contains(
                "<string>/Users/someone/.claude-self-reflect/logs/daemon.out.log</string>"
            ),
            "{text}"
        );
        assert!(!text.contains("<string>~"), "no tilde paths: {text}");
        assert!(
            !text.contains("ramakrishnanannaswamy"),
            "no owner paths: {text}"
        );
        // claude's folder first, then the binary's, then the fixed places.
        assert!(
            text.contains("<string>/opt/claude/bin:/opt/csr/bin:/Users/someone/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string>"),
            "{text}"
        );
        // A CSR_NO_* key reads as an inhibited feature in the pane.
        assert!(!text.contains("CSR_NO_"), "{text}");
        let mut spec = spec;
        spec.no_codex_import = true;
        assert!(render_daemon_plist(&spec).contains("<key>CSR_NO_CODEX_IMPORT</key>"));
    }

    #[test]
    fn the_daemon_plist_escapes_xml_in_paths() {
        let mut spec = agent_spec(Path::new("/Users/a&b"));
        spec.engine = "/Users/a&b/<bin>/csr-engine".into();
        let text = render_daemon_plist(&spec);
        assert!(
            text.contains("/Users/a&amp;b/&lt;bin&gt;/csr-engine"),
            "{text}"
        );
        assert!(!text.contains("a&b"), "{text}");
    }

    #[test]
    fn the_plist_names_the_database_the_daemon_serves() {
        // `vector_ipc::launchd_database` is what ties a client to this daemon.
        let home = tempfile::tempdir().unwrap();
        let spec = agent_spec(home.path());
        let mut fake = FakeLaunchctl::new(false);
        install(&spec, &mut fake);
        assert!(crate::search::vector_ipc::daemon_configured_for_database(
            &spec.db_path,
            Some(home.path()),
        ));
    }

    #[test]
    fn a_fresh_install_writes_the_plist_and_bootstraps_it_once() {
        let home = tempfile::tempdir().unwrap();
        let spec = agent_spec(home.path());
        let mut fake = FakeLaunchctl::new(false);
        let outcome = install(&spec, &mut fake);
        assert_eq!(outcome, DaemonOutcome::Running { rewritten: true });
        assert_eq!(
            std::fs::read_to_string(spec.plist_path()).unwrap(),
            render_daemon_plist(&spec)
        );
        assert!(home.path().join(".claude-self-reflect/logs").is_dir());
        // bootstrap runs the job (RunAtLoad); a kickstart on top would restart it.
        assert_eq!(
            fake.calls,
            vec![
                "print gui/501/com.ramakay.csr-engine.daemon",
                "bootstrap gui/501"
            ]
        );
    }

    #[test]
    fn a_second_run_leaves_the_file_alone_and_restarts_the_job() {
        let home = tempfile::tempdir().unwrap();
        let spec = agent_spec(home.path());
        install(&spec, &mut FakeLaunchctl::new(false));
        let first = std::fs::metadata(spec.plist_path())
            .unwrap()
            .modified()
            .unwrap();
        std::thread::sleep(Duration::from_millis(30));

        let mut fake = FakeLaunchctl::new(true);
        assert_eq!(
            install(&spec, &mut fake),
            DaemonOutcome::Running { rewritten: false }
        );
        let second = std::fs::metadata(spec.plist_path())
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(first, second, "an identical plist must not be rewritten");
        assert_eq!(
            fake.calls,
            vec![
                "print gui/501/com.ramakay.csr-engine.daemon",
                "kickstart -k"
            ]
        );
    }

    #[test]
    fn a_changed_install_reloads_the_job_because_kickstart_does_not_reread_it() {
        let home = tempfile::tempdir().unwrap();
        let mut spec = agent_spec(home.path());
        install(&spec, &mut FakeLaunchctl::new(false));
        spec.engine = "/opt/csr/new/csr-engine".into();

        let mut fake = FakeLaunchctl::new(true);
        assert_eq!(
            install(&spec, &mut fake),
            DaemonOutcome::Running { rewritten: true }
        );
        assert!(std::fs::read_to_string(spec.plist_path())
            .unwrap()
            .contains("/opt/csr/new/csr-engine"));
        assert_eq!(
            fake.calls,
            vec![
                "print gui/501/com.ramakay.csr-engine.daemon",
                "bootout gui/501/com.ramakay.csr-engine.daemon",
                "bootstrap gui/501"
            ]
        );
    }

    #[test]
    fn bootstrap_is_retried_while_launchd_tears_the_old_job_down() {
        let home = tempfile::tempdir().unwrap();
        let spec = agent_spec(home.path());
        let mut fake = FakeLaunchctl::new(false);
        fake.bootstrap_fails = 2;
        assert_eq!(
            install(&spec, &mut fake),
            DaemonOutcome::Running { rewritten: true }
        );
        assert_eq!(
            fake.calls
                .iter()
                .filter(|c| c.starts_with("bootstrap"))
                .count(),
            3
        );
    }

    #[test]
    fn a_bootstrap_that_keeps_failing_reports_the_launchctl_error() {
        let home = tempfile::tempdir().unwrap();
        let spec = agent_spec(home.path());
        let mut fake = FakeLaunchctl::new(false);
        fake.bootstrap_fails = 99;
        let outcome = install(&spec, &mut fake);
        assert_eq!(
            outcome,
            DaemonOutcome::Failed("Bootstrap failed: 5: Input/output error".into())
        );
        let report = daemon_report(&outcome, Path::new("/opt/csr/bin/csr-engine"));
        assert!(report.contains("Bootstrap failed: 5"), "{report}");
        assert!(
            report.contains("/opt/csr/bin/csr-engine daemon"),
            "{report}"
        );
    }

    #[test]
    fn a_hand_written_agent_with_a_live_binary_is_kept() {
        let home = tempfile::tempdir().unwrap();
        let spec = agent_spec(home.path());
        let exe = std::env::current_exe().unwrap();
        let theirs = format!(
            "<plist><dict><key>ProgramArguments</key><array><string>{}</string><string>daemon</string></array><key>EnvironmentVariables</key><dict><key>CSR_NO_RECAP</key><string>1</string></dict></dict></plist>",
            exe.display()
        );
        std::fs::create_dir_all(spec.plist_path().parent().unwrap()).unwrap();
        std::fs::write(spec.plist_path(), &theirs).unwrap();

        let mut fake = FakeLaunchctl::new(true);
        assert_eq!(install(&spec, &mut fake), DaemonOutcome::KeptExisting);
        assert_eq!(std::fs::read_to_string(spec.plist_path()).unwrap(), theirs);

        // The same file pointing at a binary that is gone is not worth keeping.
        let dead = theirs.replace(&exe.display().to_string(), "/nowhere/csr-engine");
        std::fs::write(spec.plist_path(), dead).unwrap();
        assert_eq!(
            install(&spec, &mut FakeLaunchctl::new(false)),
            DaemonOutcome::Running { rewritten: true }
        );
        assert_eq!(
            std::fs::read_to_string(spec.plist_path()).unwrap(),
            render_daemon_plist(&spec)
        );
    }

    #[test]
    fn the_opt_out_and_other_platforms_touch_nothing() {
        let home = tempfile::tempdir().unwrap();
        let spec = agent_spec(home.path());
        let mut calls = 0;
        let mut count = |_: &[&str]| {
            calls += 1;
            Ok(())
        };
        assert_eq!(
            daemon_step(&spec, true, true, 501, &mut count, &|_| {}),
            DaemonOutcome::OptedOut
        );
        assert_eq!(
            daemon_step(&spec, false, false, 501, &mut count, &|_| {}),
            DaemonOutcome::Manual
        );
        assert_eq!(calls, 0, "no launchctl call");
        assert!(!spec.plist_path().exists());

        let out = daemon_report(&DaemonOutcome::OptedOut, Path::new("/x/csr-engine"));
        assert!(
            out.contains("CSR_NO_DAEMON") && out.contains("/x/csr-engine daemon"),
            "{out}"
        );
        let manual = daemon_report(&DaemonOutcome::Manual, Path::new("/x/csr-engine"));
        assert!(manual.contains("/x/csr-engine daemon"), "{manual}");
    }

    #[test]
    fn csr_no_daemon_is_read_from_the_environment() {
        let _guard = crate::daemon::dream_cadence::env_test_guard();
        std::env::remove_var("CSR_NO_DAEMON");
        assert!(!daemon_opted_out());
        for (value, expected) in [("1", true), ("true", true), ("0", false), ("", false)] {
            std::env::set_var("CSR_NO_DAEMON", value);
            assert_eq!(daemon_opted_out(), expected, "CSR_NO_DAEMON={value:?}");
        }
        std::env::remove_var("CSR_NO_DAEMON");
    }
}
