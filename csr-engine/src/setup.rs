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

use std::path::Path;

use anyhow::Result;

use crate::engine::Engine;
use crate::hooks;
use crate::import;

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
    eprintln!("[1/6] Database directory ready: {}", csr_dir.display());
    let eng = Engine::new(db_path, projects_dir)?;
    eng.embeddings().warm()?;
    eprintln!("  Embedding model ready");

    // Step 2: Register as MCP server
    eprintln!("[2/6] Registering MCP server...");
    // A registration failure must not cost the user hooks and import, but it
    // must not be reported as success either: print it here, finish the rest,
    // and return it at the end so the exit code is non-zero and the installers
    // take their failure branch.
    let mcp_error = register_mcp_server().err();
    if let Some(e) = &mcp_error {
        eprintln!("  Error: {e}");
    }

    // Step 3: Install hooks
    eprintln!("[3/6] Installing hooks...");
    if let Err(e) = hooks::install::handle(true) {
        eprintln!("  Warning: hook installation failed: {e}");
        eprintln!("  You can run `csr-engine hook install --apply` later.");
    }

    // Step 4: Import conversations
    eprintln!("[4/6] Importing conversations...");

    // Count JSONL files for progress
    let total_files = count_total_files(projects_dir);
    eprintln!("  Found {} JSONL files to process", total_files);

    let imported = eng.import_conversations(None).await?;
    eprintln!("  Imported {} chunks", imported);

    // Step 5: Run enrichment
    eprintln!("[5/6] Running heuristic enrichment...");
    let (backfilled, enriched) = eng.backfill_and_enrich().await?;
    if backfilled > 0 {
        eprintln!("  Backfilled {} import_state rows", backfilled);
    }
    eprintln!("  Enriched {} conversations", enriched);

    // Step 6: Save Anthropic API key if provided
    if let Some(key) = &anthropic_key {
        eprintln!("[6/6] Saving Anthropic API key...");
        save_anthropic_key(csr_dir, key)?;
        eprintln!("  Saved to {}", csr_dir.join(".env").display());
    } else {
        eprintln!("[6/6] Skipping AI narratives (no --anthropic-key provided)");
    }

    // Summary
    let conversations = eng.storage().count_conversations().unwrap_or(0);
    let reflections = eng.storage().count_reflection_embeddings().unwrap_or(0);
    let projects = eng.storage().count_projects().unwrap_or(0);

    if let Some(e) = mcp_error {
        eprintln!("\n=== Setup Incomplete ===\n");
        eprintln!("  Conversations: {}", conversations);
        eprintln!("  Reflections:   {}", reflections);
        eprintln!("  Projects:      {}", projects);
        eprintln!();
        eprintln!("  Hooks and import are done. The MCP server is NOT registered:");
        eprintln!("  {e}");
        eprintln!();
        return Err(e);
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

fn mcp_registration_error(binary_str: &str, stderr: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "could not register the MCP server: {}\n  Run these yourself, then restart Claude Code:\n    claude mcp remove {} -s user\n    claude mcp add {} {} -s user",
        stderr.trim(),
        MCP_SERVER_NAME,
        MCP_SERVER_NAME,
        binary_str
    )
}

/// Register csr-engine as an MCP server with Claude Code.
///
/// An upgrade installs a new binary at a new absolute path while the user-scope
/// registration still names the old one, and `claude mcp add` refuses to
/// overwrite it. Failing quietly there is how setup used to print "Setup
/// Complete" while Claude Code kept launching the binary that was just
/// replaced — the `write_mcp_config` fallback below writes `mcpServers` into
/// ~/.claude/settings.json, which Claude Code does not read for MCP at all.
/// So: remove-then-add once, and surface anything else as an error.
fn register_mcp_server() -> Result<()> {
    let binary_path = std::env::current_exe()?;
    let binary_str = binary_path.to_string_lossy().to_string();

    let output = match mcp_add(&binary_str) {
        Ok(output) => output,
        Err(_) => {
            // The `claude` binary could not be spawned at all — the one case
            // the direct-write fallback still covers.
            eprintln!("  `claude` CLI not found, writing MCP config directly...");
            return write_mcp_config(&binary_str);
        }
    };

    if output.status.success() {
        eprintln!("  Registered via `claude mcp add`");
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if classify_add_failure(&stderr) == AddFailure::Fatal {
        return Err(mcp_registration_error(&binary_str, &stderr));
    }

    eprintln!("  Existing registration found — repointing it at {binary_str}");
    let removed = std::process::Command::new("claude")
        .args(["mcp", "remove", MCP_SERVER_NAME, "-s", "user"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();
    if let Err(e) = removed {
        return Err(mcp_registration_error(&binary_str, &e.to_string()));
    }

    // Exactly one retry: a second failure is a real problem, not a race.
    match mcp_add(&binary_str) {
        Ok(retry) if retry.status.success() => {
            eprintln!("  Registered via `claude mcp add`");
            Ok(())
        }
        Ok(retry) => Err(mcp_registration_error(
            &binary_str,
            &String::from_utf8_lossy(&retry.stderr),
        )),
        Err(e) => Err(mcp_registration_error(&binary_str, &e.to_string())),
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
        if let Ok(files) = import::list_jsonl_files(dir) {
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
    fn test_mcp_registration_error_names_both_commands() {
        let msg = mcp_registration_error("/home/me/.local/bin/csr-engine", "boom").to_string();
        assert!(
            msg.contains("claude mcp remove claude-self-reflect -s user"),
            "{msg}"
        );
        assert!(
            msg.contains(
                "claude mcp add claude-self-reflect /home/me/.local/bin/csr-engine -s user"
            ),
            "{msg}"
        );
        assert!(msg.contains("boom"), "{msg}");
    }
}
