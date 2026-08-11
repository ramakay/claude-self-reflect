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

    // Step 1: Ensure DB directory exists
    let csr_dir = db_path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(csr_dir)?;
    eprintln!("[1/7] Database directory ready: {}", csr_dir.display());

    // Step 2: Register as MCP server (before Engine::new — works without DB)
    eprintln!("[2/7] Registering MCP server...");
    register_mcp_server()?;

    // Step 3: Install hooks
    eprintln!("[3/7] Installing hooks...");
    if let Err(e) = hooks::install::handle(true) {
        eprintln!("  Warning: hook installation failed: {e}");
        eprintln!("  You can run `csr-engine hook install --apply` later.");
    }

    // Step 4: Create engine, import conversations
    eprintln!("[4/7] Importing conversations...");
    let eng = Engine::new(db_path, projects_dir)?;

    // Count JSONL files for progress
    let total_files = count_total_files(projects_dir);
    eprintln!("  Found {} JSONL files to process", total_files);

    let imported = eng.import_conversations(None).await?;
    eprintln!("  Imported {} chunks", imported);

    // Optional vendor corpus: absence is intentionally silent/inert.
    if let Some(codex_root) = dirs::home_dir().map(|home| home.join(".codex/sessions")) {
        if codex_root.exists() {
            let adapter_engine = eng.clone();
            let stats = tokio::task::spawn_blocking(move || {
                crate::import::codex_rollout::import_changed_rollouts(&adapter_engine, &codex_root)
            })
            .await??;
            eprintln!(
                "  Imported {} Codex rollout chunks from {} changed files",
                stats.chunks_imported, stats.files_imported
            );
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

/// Register csr-engine as an MCP server with Claude Code.
/// Tries `claude mcp add` first, falls back to direct settings.json write.
fn register_mcp_server() -> Result<()> {
    let binary_path = std::env::current_exe()?;
    let binary_str = binary_path.to_string_lossy().to_string();

    // Try `claude mcp add` first
    let result = std::process::Command::new("claude")
        .args([
            "mcp",
            "add",
            "claude-self-reflect",
            &binary_str,
            "-s",
            "user",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();

    match result {
        Ok(output) if output.status.success() => {
            eprintln!("  Registered via `claude mcp add`");
            Ok(())
        }
        _ => {
            // Fallback: write directly to settings.json
            eprintln!("  `claude` CLI not found, writing MCP config directly...");
            write_mcp_config(&binary_str)
        }
    }
}

/// Write MCP server config directly to ~/.claude/settings.json.
fn write_mcp_config(binary_path: &str) -> Result<()> {
    let settings_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?
        .join(".claude")
        .join("settings.json");

    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content)?
    } else {
        serde_json::json!({})
    };

    // Add MCP server entry
    let mcp_servers = settings
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("settings.json is not an object"))?
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));

    mcp_servers["claude-self-reflect"] = serde_json::json!({
        "command": binary_path,
        "args": [],
        "scope": "user"
    });

    // Atomic write
    let tmp_path = settings_path.with_extension("json.tmp");
    std::fs::write(&tmp_path, serde_json::to_string_pretty(&settings)?)?;
    std::fs::rename(&tmp_path, &settings_path)?;

    eprintln!("  MCP config written to {}", settings_path.display());
    Ok(())
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
}
