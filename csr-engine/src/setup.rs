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

use std::io::{BufRead, IsTerminal};
use std::path::Path;

use anyhow::Result;

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
    if let Err(e) = hooks::install::handle(true) {
        eprintln!("  Warning: hook installation failed: {e}");
        eprintln!("  You can run `csr-engine hook install --apply` later.");
    }

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

    if let Some(failure) = mcp_error {
        eprintln!("\n=== Setup Incomplete ===\n");
        eprintln!("  Conversations: {}", conversations);
        eprintln!("  Reflections:   {}", reflections);
        eprintln!("  Projects:      {}", projects);
        eprintln!();
        eprintln!("  Hooks and import are done. {}", failure.summary);
        eprintln!("  {}", failure.detail);
        eprintln!();
        // Short on purpose: the detail above is the only copy the user reads.
        return Err(anyhow::anyhow!("MCP server not registered"));
    }

    eprintln!("\n=== Setup Complete ===\n");
    eprintln!("  Conversations: {}", conversations);
    eprintln!("  Reflections:   {}", reflections);
    eprintln!("  Projects:      {}", projects);
    eprintln!();
    eprintln!("Next steps:");
    eprintln!("  1. Restart Claude Code to activate MCP tools");
    eprintln!("  2. Try: reflect_on_past(\"what did we work on?\")");
    if anthropic_key.is_some() {
        eprintln!("  3. Run `csr-engine daemon` for AI-powered narrative enrichment");
    } else {
        eprintln!("  3. Optional: `csr-engine setup --anthropic-key=sk-ant-...` for AI narratives");
    }
    eprintln!();

    Ok(())
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
}
