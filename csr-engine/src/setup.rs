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

    // Step 1: Ensure DB directory exists
    let csr_dir = db_path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(csr_dir)?;
    eprintln!("[1/6] Database directory ready: {}", csr_dir.display());

    // Step 2: Register as MCP server (before Engine::new — works without DB)
    eprintln!("[2/6] Registering MCP server...");
    register_mcp_server()?;

    // Step 3: Install hooks
    eprintln!("[3/6] Installing hooks...");
    if let Err(e) = hooks::install::handle(true) {
        eprintln!("  Warning: hook installation failed: {e}");
        eprintln!("  You can run `csr-engine hook install --apply` later.");
    }

    // Step 4: Create engine, import conversations
    eprintln!("[4/6] Importing conversations...");
    let eng = Engine::new(db_path, projects_dir)?;

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
}
