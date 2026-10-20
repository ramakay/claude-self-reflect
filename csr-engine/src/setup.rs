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
//! After those steps succeed, setup installs the csr-companion plugin through
//! Claude Code's own plugin CLI (`--no-companion` skips it). A failure there is
//! reported with the two manual `/plugin` lines and never fails setup.
//!
//! `--relay` takes another path: the binary writes the plugin as a local marketplace under
//! `~/.claude-self-reflect/csr-companion/marketplace` and installs it from there
//! (`install_relay`, also `csr-engine companion plugin install`), so the plugin's version is
//! always the engine's.

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
    relay: bool,
) -> Result<()> {
    eprintln!("\n=== Claude Self-Reflect Setup ===\n");

    // Step 1: Ensure DB directory exists, open the engine, and load the
    // embedding model. The model loads lazily on first embed everywhere else;
    // here a first-run download belongs in front of the user (with progress),
    // and it has to succeed BEFORE the MCP server and hooks are written into
    // Claude's config, so a failed download never leaves setup half-applied.
    let csr_dir = db_path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(csr_dir)?;
    eprintln!("[1/7] Database directory ready: {}", csr_dir.display());
    let eng = Engine::new(db_path, projects_dir)?;
    eng.embeddings().warm()?;
    eprintln!("  Embedding model ready");

    // Step 2: Register as MCP server
    eprintln!("[2/7] Registering MCP server...");
    // A registration failure must not cost the user hooks and import, but it
    // must not be reported as success either: note it here, finish the rest,
    // and print the details once at the end. The returned error is deliberately
    // one line so the caller's own rendering does not repeat the remedy.
    let mcp_error = register_mcp_server().err();
    if mcp_error.is_some() {
        eprintln!("  MCP registration failed; details at the end of setup.");
    }

    // Step 3: Install hooks
    eprintln!("[3/7] Installing hooks...");
    // Like the MCP step: a failure does not cost the user the import, but it
    // is carried to the summary instead of being dropped after this warning.
    let hook_error = hook_install_step(|| hooks::install::handle(true));

    // Step 4: Import conversations
    eprintln!("[4/7] Importing conversations...");

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
    eprintln!("[5/7] Running heuristic enrichment...");
    let (backfilled, enriched) = eng.backfill_and_enrich().await?;
    if backfilled > 0 {
        eprintln!("  Backfilled {} import_state rows", backfilled);
    }
    eprintln!("  Enriched {} conversations", enriched);

    // Step 6: Save Anthropic API key if provided
    if let Some(key) = &anthropic_key {
        eprintln!("[6/7] Saving Anthropic API key...");
        save_anthropic_key(csr_dir, key)?;
        eprintln!("  Saved to {}", csr_dir.join(".env").display());
    } else {
        eprintln!("[6/7] Skipping AI narratives (no --anthropic-key provided)");
    }

    // Step 7: Dreaming consent (Journal v4 P5, locked decision 15). Presented
    // ON by default, with a per-night token estimate computed from the corpus
    // that was just imported — i.e. from real numbers, shown BEFORE the
    // question is asked.
    dreaming_consent_step(eng.storage())?;

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
    if relay {
        let report = tokio::task::spawn_blocking(move || {
            let home = dirs::home_dir().unwrap_or_default();
            let engine = std::env::current_exe().unwrap_or_default();
            relay_step(
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
        eprint!("{}", next_steps_relay(report.as_ref()));
        return Ok(());
    }
    let marketplace = companion_marketplace();
    let companion_outcome = tokio::task::spawn_blocking(move || {
        companion_step(
            companion,
            Path::new("claude"),
            &marketplace,
            COMPANION_TIMEOUT,
        )
    })
    .await
    .unwrap_or_else(|e| {
        Some(CompanionOutcome::Failed {
            reason: format!("the install task did not finish: {e}"),
        })
    });

    eprintln!("\n=== Setup Complete ===\n");
    eprintln!("  Conversations: {}", conversations);
    eprintln!("  Reflections:   {}", reflections);
    eprintln!("  Projects:      {}", projects);
    eprintln!();
    eprint!("{}", next_steps(companion_outcome.as_ref()));

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

/// The two commands that install the companion plugin (the pane, and the dreamer's
/// "what if...?"). The plugin reaches users through this repository's marketplace
/// (`.claude-plugin/marketplace.json`, name `claude-self-reflect`, plugin
/// `csr-companion`). Setup installs the plugin itself (`install_companion`); these
/// are the manual fallback it prints, as the last two lines of the output, when it
/// was skipped (`--no-companion`) or could not install.
fn companion_install_instructions() -> &'static str {
    "/plugin marketplace add ramakay/claude-self-reflect\n/plugin install csr-companion@claude-self-reflect"
}

/// The marketplace source handed to `claude plugin install --marketplace`.
const DEFAULT_COMPANION_MARKETPLACE: &str = "ramakay/claude-self-reflect";

/// Hard stop for each `claude plugin` call. Only that child is killed.
const COMPANION_TIMEOUT: Duration = Duration::from_secs(120);

fn companion_marketplace() -> String {
    match std::env::var("CSR_COMPANION_MARKETPLACE") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => DEFAULT_COMPANION_MARKETPLACE.to_string(),
    }
}

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

const COMPANION_PLUGIN: &str = "csr-companion";

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

/// The installed csr-companion entry in `claude plugin list --json`, if any.
fn find_listed(run: &ClaudeRun) -> Option<CompanionInfo> {
    let list = first_json(&run.stdout, |v| v.is_array())?;
    list.as_array()?
        .iter()
        .filter_map(info_from)
        .find(|i| i.plugin == COMPANION_PLUGIN)
}

/// Install csr-companion through Claude Code's plugin CLI. The binary path, the
/// marketplace source and the timeout are parameters so tests never touch PATH or
/// the process environment. Never panics and never returns an error: every
/// problem is a `Failed { reason }` with a one-line reason.
pub fn install_companion(claude: &Path, marketplace: &str, timeout: Duration) -> CompanionOutcome {
    let fail = |reason: String| CompanionOutcome::Failed { reason };

    // Already there? A list that cannot be read is not fatal: try the install.
    match run_with_timeout(claude, &["plugin", "list", "--json"], timeout) {
        Ok(run) if run.success => {
            if let Some(info) = find_listed(&run) {
                return CompanionOutcome::AlreadyInstalled(info);
            }
        }
        Ok(_) => {}
        Err(reason) => return fail(reason),
    }

    let run = match run_with_timeout(
        claude,
        &[
            "plugin",
            "install",
            COMPANION_PLUGIN,
            "--marketplace",
            marketplace,
            "--json",
        ],
        timeout,
    ) {
        Ok(run) => run,
        Err(reason) => return fail(reason),
    };

    let result = first_json(&run.stdout, |v| v.is_object());
    if !run.success {
        let detail = result
            .as_ref()
            .and_then(|v| v.get("message"))
            .and_then(|m| m.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| run.stderr.trim().to_string());
        return fail(format!(
            "`claude plugin install` failed: {}",
            one_line(&detail)
        ));
    }
    let Some(result) = result else {
        return fail("`claude plugin install` returned no JSON result".to_string());
    };
    if result.get("outcome").and_then(|o| o.as_str()) != Some("ok") {
        let message = result
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("outcome was not ok");
        return fail(format!(
            "`claude plugin install` did not succeed: {}",
            one_line(message)
        ));
    }
    let Some(mut info) = info_from(&result) else {
        return fail("`claude plugin install` JSON named no plugin".to_string());
    };

    // The install result carries no version; the list does. Ask once more, and
    // say "unknown" rather than guess if that fails too.
    if info.version == "unknown" {
        if let Ok(list) = run_with_timeout(claude, &["plugin", "list", "--json"], timeout) {
            if let Some(listed) = find_listed(&list) {
                info.version = listed.version;
            }
        }
    }
    CompanionOutcome::Installed(info)
}

// ── the relay plugin: written by this binary, installed from a local marketplace ──

/// The plugin id Claude Code lists: `csr-companion@claude-self-reflect`.
fn relay_plugin_id() -> String {
    format!("{}@{}", emit::PLUGIN_NAME, emit::MARKETPLACE_NAME)
}

/// What `companion plugin install` and `setup --relay` did, step by step.
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
fn configured_options(claude: &Path, timeout: Duration) -> Vec<(String, String)> {
    let id = relay_plugin_id();
    let args = ["plugin", "configure", id.as_str(), "--json"];
    let Ok(run) = run_ok(claude, &args, timeout, "configure") else {
        return Vec::new();
    };
    let Some(info) = first_json(&run.stdout, |v| v.is_object()) else {
        return Vec::new();
    };
    info["configured"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|key| {
            let key = key.as_str()?;
            Some((key.to_string(), info["inputs"][key].as_str()?.to_string()))
        })
        .collect()
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
        let disable = ["plugin", "disable", id.as_str(), "--json"];
        run_outcome_ok(claude, &disable, timeout, "disable").map_err(|e| {
            format!("{e}; the plugin is installed and enabled, run `claude plugin disable {id}`")
        })?;
    }
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
    let mut options = Vec::new();
    let mut enabled = true;
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
                options = configured_options(claude, timeout);
                // Removing the marketplace uninstalls the plugin; remember if it was off.
                enabled = relay_enabled(
                    list_plugins(claude, timeout)?
                        .iter()
                        .find(|e| e["id"].as_str() == Some(relay_plugin_id().as_str())),
                );
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
    let listed = list_plugins(claude, timeout)?;
    match relay_entry(&listed) {
        None => {
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
                if options.is_empty() {
                    options = configured_options(claude, timeout);
                }
                let enabled = relay_enabled(Some(entry));
                let uninstall = ["plugin", "uninstall", id.as_str(), "--keep-data", "--json"];
                run_outcome_ok(claude, &uninstall, timeout, "uninstall")?;
                install_relay_plugin(claude, &options, enabled, timeout)?;
                st.steps
                    .push(format!("plugin reinstalled: {was} -> {version}"));
            }
        }
    }

    let listed = list_plugins(claude, timeout)?;
    let entry = relay_entry(&listed)
        .ok_or_else(|| format!("`claude plugin list` does not show {id} after the install"))?;
    if !relay_is_current(entry, &market.relay) {
        return Err(format!(
            "{id} is installed but its cache is not this engine's plugin; \
             run `claude plugin uninstall {id}` and then this again"
        ));
    }
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

/// The relay companion step of setup. `enabled == false` (`--no-companion`) makes no `claude`
/// call and writes nothing.
fn relay_step(
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

/// The companion step of setup. `enabled == false` (`--no-companion`) makes no
/// `claude` call at all and returns `None`.
fn companion_step(
    enabled: bool,
    claude: &Path,
    marketplace: &str,
    timeout: Duration,
) -> Option<CompanionOutcome> {
    if !enabled {
        return None;
    }
    eprintln!("Installing the csr-companion plugin through `claude plugin`...");
    Some(install_companion(claude, marketplace, timeout))
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

/// What a successful setup ends with: the next steps, then the companion's
/// receipt, or its manual install lines when it was skipped or failed.
/// `companion` is `None` when `--no-companion` skipped the install.
fn next_steps(companion: Option<&CompanionOutcome>) -> String {
    let manual = |lead: String| {
        format!(
            "{lead}\nOptional: the CSR companion pane. In Claude Code, run:\n{}\n",
            companion_install_instructions()
        )
    };
    let companion_text = match companion {
        Some(CompanionOutcome::Installed(i)) => format!(
            "companion: {} {} from {}, scope {}\nRun /reload-plugins in Claude Code (or restart it) to load it.\n",
            i.plugin, i.version, i.marketplace, i.scope
        ),
        Some(CompanionOutcome::AlreadyInstalled(i)) => format!(
            "companion: {} {} from {}, scope {} (already installed)\nRun /reload-plugins in Claude Code (or restart it) if it is not loaded.\n",
            i.plugin, i.version, i.marketplace, i.scope
        ),
        Some(CompanionOutcome::Failed { reason }) => {
            manual(format!("companion: not installed: {reason}"))
        }
        None => manual("companion: skipped (--no-companion)".to_string()),
    };
    next_steps_text(&companion_text)
}

fn next_steps_text(companion_text: &str) -> String {
    format!(
        "Next steps:\n  1. Restart Claude Code to activate MCP tools\n  2. Open Claude Code in a project you have worked in; its first line recaps where you left off\n  3. Ask in plain words, e.g. \"what did we work on last week?\"\n\n{companion_text}"
    )
}

/// `next_steps` for `setup --relay`: the receipt, `/reload-plugins` when something changed,
/// and the one command that retries the install. `None` is `--no-companion`.
fn next_steps_relay(report: Option<&RelayReport>) -> String {
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
        Some((CompanionOutcome::Failed { reason }, _)) => {
            format!("companion: not installed: {reason}\n{retry}")
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

    eprintln!("[7/7] Dreaming (overnight review of your open work)");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The repository's marketplace, read from the checkout the engine is built in.
    fn marketplace() -> serde_json::Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../.claude-plugin/marketplace.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn setup_companion_install_lines_match_marketplace() {
        let market = marketplace();
        let name = market["name"].as_str().unwrap();
        let plugin = &market["plugins"][0];
        let plugin_name = plugin["name"].as_str().unwrap();
        let lines: Vec<&str> = companion_install_instructions().lines().collect();
        assert_eq!(
            lines,
            [
                "/plugin marketplace add ramakay/claude-self-reflect",
                "/plugin install csr-companion@claude-self-reflect",
            ]
        );
        // the install line names the marketplace's own name and its plugin, and that plugin is the folder shipped here
        assert_eq!(lines[1], format!("/plugin install {plugin_name}@{name}"));
        assert_eq!(plugin["source"], "./plugins/csr-companion");
        assert!(Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/csr-companion/.claude-plugin/plugin.json")
            .is_file());
    }

    // ── companion install (fake `claude`, injected path; no env or PATH mutation) ──

    const TWO_LINES: [&str; 2] = [
        "/plugin marketplace add ramakay/claude-self-reflect",
        "/plugin install csr-companion@claude-self-reflect",
    ];
    const LIST_ENTRY: &str = r#"[{"id":"csr-companion@claude-self-reflect","version":"0.5.0","scope":"user","enabled":true}]"#;
    const INSTALL_OK: &str = r#"{"command":"install","outcome":"ok","plugin":"csr-companion@claude-self-reflect","pluginId":"csr-companion@claude-self-reflect","scope":"user","message":"Successfully installed"}"#;

    struct Fake {
        _dir: tempfile::TempDir,
        bin: std::path::PathBuf,
        log: std::path::PathBuf,
        pid: std::path::PathBuf,
    }

    impl Fake {
        /// `list_body` / `install_body` are shell snippets run for the matching
        /// subcommand. Every call appends its argv to the log first.
        fn new(list_body: &str, install_body: &str) -> Fake {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("claude");
            let log = dir.path().join("argv.log");
            let pid = dir.path().join("pid");
            let script = format!(
                "#!/bin/sh\necho \"$@\" >> '{log}'\necho $$ > '{pid}'\ncase \"$2\" in\n  list) {list_body} ;;\n  install) {install_body} ;;\nesac\n",
                log = log.display(),
                pid = pid.display(),
            );
            std::fs::write(&bin, script).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            Fake {
                _dir: dir,
                bin,
                log,
                pid,
            }
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    fn tail_two(text: &str) -> Vec<&str> {
        let mut t: Vec<&str> = text.lines().rev().take(2).collect();
        t.reverse();
        t
    }

    fn stateful_fake() -> Fake {
        // list shows the plugin only after install has run (marker file next to the log)
        let fake = Fake::new("true", "true");
        let marker = fake.log.with_file_name("installed");
        let script = std::fs::read_to_string(&fake.bin).unwrap().replace(
            "  list) true ;;\n  install) true ;;",
            &format!(
                "  list) if [ -f '{m}' ]; then echo '{LIST_ENTRY}'; else echo '[]'; fi ;;\n  install) touch '{m}'; echo 'warming up'; echo '{INSTALL_OK}' ;;",
                m = marker.display()
            ),
        );
        std::fs::write(&fake.bin, script).unwrap();
        fake
    }

    #[test]
    fn companion_install_success_parses_the_json_line_and_prints_the_receipt() {
        let fake = stateful_fake();
        let outcome = install_companion(&fake.bin, "/some/marketplace", Duration::from_secs(10));
        let CompanionOutcome::Installed(info) = &outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(
            info,
            &CompanionInfo {
                plugin: "csr-companion".into(),
                version: "0.5.0".into(),
                marketplace: "claude-self-reflect".into(),
                scope: "user".into(),
            }
        );
        let calls = fake.calls();
        assert_eq!(calls[0], "plugin list --json");
        assert_eq!(
            calls[1],
            "plugin install csr-companion --marketplace /some/marketplace --json"
        );
        let text = next_steps(Some(&outcome));
        assert!(
            text.contains("companion: csr-companion 0.5.0 from claude-self-reflect, scope user\nRun /reload-plugins in Claude Code (or restart it) to load it.\n"),
            "{text}"
        );
        assert!(!text.contains("installs nothing"), "{text}");
        assert!(!text.contains("/plugin install"), "{text}");
    }

    #[test]
    fn companion_already_installed_never_calls_install() {
        let fake = Fake::new(&format!("echo '{LIST_ENTRY}'"), "exit 9");
        let outcome = install_companion(&fake.bin, "x", Duration::from_secs(10));
        assert!(
            matches!(&outcome, CompanionOutcome::AlreadyInstalled(i) if i.version == "0.5.0" && i.scope == "user" && i.marketplace == "claude-self-reflect"),
            "{outcome:?}"
        );
        assert_eq!(fake.calls(), vec!["plugin list --json".to_string()]);
        let text = next_steps(Some(&outcome));
        assert!(
            text.contains("companion: csr-companion 0.5.0 from claude-self-reflect, scope user (already installed)"),
            "{text}"
        );
        assert!(!text.contains("/plugin install"), "{text}");
    }

    #[test]
    fn companion_multiline_pretty_list_is_parsed() {
        let pretty = "[\\n  {\\n    \"id\": \"other@m\",\\n    \"version\": \"1\"\\n  },\\n  {\\n    \"id\": \"csr-companion@claude-self-reflect\",\\n    \"version\": \"0.5.0\",\\n    \"scope\": \"user\"\\n  }\\n]";
        let fake = Fake::new(&format!("printf '{pretty}'"), "exit 9");
        let outcome = install_companion(&fake.bin, "x", Duration::from_secs(10));
        assert!(
            matches!(&outcome, CompanionOutcome::AlreadyInstalled(i) if i.version == "0.5.0"),
            "{outcome:?}"
        );
    }

    fn assert_failed_with_manual_lines(outcome: &CompanionOutcome, needle: &str) {
        let CompanionOutcome::Failed { reason } = outcome else {
            panic!("expected Failed, got {outcome:?}");
        };
        assert!(reason.contains(needle), "{reason}");
        assert_eq!(reason.lines().count(), 1, "one line: {reason}");
        let text = next_steps(Some(outcome));
        assert!(
            text.contains(&format!("companion: not installed: {reason}\n")),
            "{text}"
        );
        assert_eq!(tail_two(&text), TWO_LINES, "{text}");
    }

    #[test]
    fn companion_missing_binary_fails_soft_with_the_manual_lines() {
        let outcome = install_companion(
            Path::new("/nonexistent-csr-test-dir/claude"),
            "x",
            Duration::from_secs(5),
        );
        assert_failed_with_manual_lines(&outcome, "could not run");
    }

    #[test]
    fn companion_nonzero_exit_fails_soft_with_the_manual_lines() {
        let fake = Fake::new(
            "echo '[]'",
            r#"echo '{"command":"install","outcome":"failed","message":"Plugin not found in marketplace"}'; echo 'boom' >&2; exit 1"#,
        );
        let outcome = install_companion(&fake.bin, "x", Duration::from_secs(10));
        assert_failed_with_manual_lines(&outcome, "Plugin not found in marketplace");
    }

    #[test]
    fn companion_bad_json_fails_soft_with_the_manual_lines() {
        let fake = Fake::new("echo '[]'", "echo 'this is not json'");
        let outcome = install_companion(&fake.bin, "x", Duration::from_secs(10));
        assert_failed_with_manual_lines(&outcome, "no JSON result");
    }

    #[test]
    fn companion_outcome_not_ok_fails_soft() {
        let fake = Fake::new(
            "echo '[]'",
            r#"echo '{"command":"install","outcome":"failed","message":"nope"}'"#,
        );
        let outcome = install_companion(&fake.bin, "x", Duration::from_secs(10));
        assert_failed_with_manual_lines(&outcome, "nope");
    }

    #[test]
    fn companion_timeout_kills_only_the_child_and_fails_soft() {
        // `exec sleep` keeps the recorded pid equal to the process we spawned. The
        // timeout leaves a fresh script time to start and write its pid on a loaded
        // machine; at 1 s a busy build host killed it before its first line.
        let fake = Fake::new("exec sleep 30", "exec sleep 30");
        let started = Instant::now();
        let outcome = install_companion(&fake.bin, "x", Duration::from_secs(3));
        let waited = started.elapsed();
        assert!(
            waited < Duration::from_secs(10),
            "did not stop at the timeout: {waited:?}"
        );
        assert_failed_with_manual_lines(&outcome, "did not finish within");
        // On a loaded machine the limit can pass before the shell runs its
        // first line: then the kill hit the shell itself, and `wait` reaped it.
        let pid: i32 = match std::fs::read_to_string(&fake.pid) {
            Ok(s) => s.trim().parse().unwrap(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => panic!("{e}"),
        };
        // SAFETY: signal 0 only probes for the pid's existence.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "child {pid} is still running after the timeout");
    }

    #[test]
    fn no_companion_flag_calls_neither_list_nor_install() {
        let fake = stateful_fake();
        let outcome = companion_step(false, &fake.bin, "x", Duration::from_secs(5));
        assert_eq!(outcome, None);
        assert!(
            fake.calls().is_empty(),
            "claude was called: {:?}",
            fake.calls()
        );
        let text = next_steps(outcome.as_ref());
        assert!(
            text.contains("companion: skipped (--no-companion)"),
            "{text}"
        );
        assert_eq!(tail_two(&text), TWO_LINES);
        // and enabled does call it
        let ran = companion_step(true, &fake.bin, "x", Duration::from_secs(10));
        assert!(
            matches!(ran, Some(CompanionOutcome::Installed(_))),
            "{ran:?}"
        );
    }

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
    fn setup_next_steps_end_with_companion_install_lines() {
        let text = next_steps(None);
        assert!(text.starts_with("Next steps:\n  1. Restart Claude Code"));
        assert!(text.ends_with(&format!("{}\n", companion_install_instructions())));
        let tail: Vec<&str> = text.lines().rev().take(2).collect();
        assert_eq!(
            tail,
            [
                "/plugin install csr-companion@claude-self-reflect",
                "/plugin marketplace add ramakay/claude-self-reflect"
            ]
        );
        // the steps name a recap and a plain-words question, not a tool or a key
        assert!(text.contains("its first line recaps where you left off"));
        assert!(text.contains("\"what did we work on last week?\""));
        assert!(!text.contains("reflect_on_past"), "{text}");
        assert!(!text.contains("--anthropic-key"), "{text}");
        assert!(!text.contains("csr-engine daemon"), "{text}");
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
  disable) echo false > "$D/enabled"
    if [ -f "$D/disable_fails" ]; then echo "settings are read-only" >&2; exit 1; fi
    echo '{{"command":"disable","outcome":"ok","plugin":"{id}","scope":"user","message":"ok"}}' ;;
  install) echo current > "$D/plugin"; echo true > "$D/enabled"
    echo '{{"command":"install","outcome":"ok","plugin":"{id}","scope":"user","message":"ok"}}' ;;
  update)
    if [ "$(cat "$D/update_refreshes")" = true ]; then echo current > "$D/plugin"; fi
    echo '{{"command":"update","outcome":"ok","plugin":"{id}","scope":"user","message":"ok"}}' ;;
  uninstall) echo absent > "$D/plugin"
    echo '{{"command":"uninstall","outcome":"ok","plugin":"{id}","scope":"user","message":"ok"}}' ;;
  configure)
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
        let dir = Path::new("/private/tmp/csr-sbx/nowhere");
        let json = |text: &str| serde_json::from_str::<serde_json::Value>(text).unwrap();
        assert_eq!(market_state(&json("[]"), dir), MarketState::Absent);
        assert_eq!(
            market_state(&json(r#"[{"name":"other","source":"github"}]"#), dir),
            MarketState::Absent
        );
        assert_eq!(
            market_state(
                &json(
                    r#"[{"name":"claude-self-reflect","source":"directory","path":"/private/tmp/csr-sbx/nowhere"}]"#
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
        let text = next_steps_relay(Some(&report));
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
        assert!(!next_steps_relay(Some(&report)).contains("/reload-plugins"));
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
        let text = next_steps_relay(Some(&report));
        assert!(text.contains("companion: not installed:"), "{text}");
    }

    #[test]
    fn relay_install_removes_and_adds_when_add_over_the_name_is_refused() {
        let fake = RelayFake::new(ON_GITHUB, "absent", true, true);
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
        let text = next_steps_relay(Some(&report));
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
    fn no_companion_with_relay_writes_nothing_and_calls_nothing() {
        let fake = RelayFake::new(NONE_REGISTERED, "absent", true, false);
        let before = std::fs::read_dir(fake.home.path()).unwrap().count();
        let report = relay_step(
            false,
            &fake.bin,
            fake.home.path(),
            Path::new(ENGINE),
            Duration::from_secs(5),
        );
        assert_eq!(report, None);
        assert!(fake.calls().is_empty());
        assert_eq!(std::fs::read_dir(fake.home.path()).unwrap().count(), before);
        assert!(next_steps_relay(None).contains("companion: skipped (--no-companion)"));
    }
}
