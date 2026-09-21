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
    // must not be reported as success either: note it here, finish the rest,
    // and print the details once at the end. The returned error is deliberately
    // one line so the caller's own rendering does not repeat the remedy.
    let mcp_error = register_mcp_server().err();
    if mcp_error.is_some() {
        eprintln!("  MCP registration failed; details at the end of setup.");
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
    /// Removed and put back. `exact` is false when only `command` and `args`
    /// could be replayed, so `env` and any other fields are gone.
    Restored { command: String, exact: bool },
    /// Removed, and putting it back also failed. The user is unregistered and
    /// has to be told exactly that.
    Lost(String),
}

impl PriorEntry {
    /// True when an entry under our name is present right now — in which case
    /// a bare `claude mcp add` would just fail with "already exists" again.
    fn still_registered(&self) -> bool {
        matches!(self, PriorEntry::Intact(_) | PriorEntry::Restored { .. })
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

    let summary = match prior {
        PriorEntry::None => "The MCP server is NOT registered:".to_string(),
        PriorEntry::Intact(Some(old)) => {
            detail.push_str(&format!(
                "\n  The previous registration ({old}) was left exactly as it was."
            ));
            format!("The MCP server is still registered to the previous binary ({old}), not the new one:")
        }
        PriorEntry::Intact(None) => {
            detail.push_str("\n  The previous registration was left exactly as it was.");
            "The MCP server is still registered to the previous binary, not the new one:"
                .to_string()
        }
        PriorEntry::Restored { command, exact } => {
            if *exact {
                detail.push_str(&format!(
                    "\n  The previous registration ({command}) was put back unchanged, so Claude Code still works — with the old binary."
                ));
            } else {
                detail.push_str(&format!(
                    "\n  The previous registration ({command}) was put back from its command and args only; any env vars it had were not restored."
                ));
            }
            format!("The MCP server is still registered to the previous binary ({command}), not the new one:")
        }
        PriorEntry::Lost(old) => {
            detail.push_str(&format!(
                "\n  The previous registration ({old}) was removed and could not be restored. Nothing is registered right now."
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

    Err(mcp_registration_error(
        &binary_str,
        &retry_stderr,
        &restore_previous(&previous),
    ))
}

/// Put back the entry we removed, whole.
///
/// `claude mcp add-json` replays the captured object exactly — `args`, `env`
/// and anything else Claude Code stored. Older Claude Code releases have no
/// `add-json`, so fall back to `claude mcp add` with the command and args,
/// which loses `env`; the message says so. One attempt each, and the result is
/// reported either way since the caller is already returning an error.
fn restore_previous(previous: &serde_json::Value) -> PriorEntry {
    let command = entry_command(previous).unwrap_or_else(|| "unknown".to_string());

    if let Ok(json) = serde_json::to_string(previous) {
        let restored = std::process::Command::new("claude")
            .args(["mcp", "add-json", MCP_SERVER_NAME, &json, "-s", "user"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output();
        if matches!(restored, Ok(ref out) if out.status.success()) {
            return PriorEntry::Restored {
                command,
                exact: true,
            };
        }
    }

    let mut args = vec![
        "mcp".to_string(),
        "add".to_string(),
        MCP_SERVER_NAME.to_string(),
        command.clone(),
        "-s".to_string(),
        "user".to_string(),
    ];
    if let Some(extra) = previous.get("args").and_then(|a| a.as_array()) {
        for value in extra {
            if let Some(arg) = value.as_str() {
                args.push(arg.to_string());
            }
        }
    }
    let fallback = std::process::Command::new("claude")
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();
    if matches!(fallback, Ok(ref out) if out.status.success()) {
        return PriorEntry::Restored {
            command,
            exact: false,
        };
    }

    PriorEntry::Lost(command)
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
                command: "/old/csr-engine".to_string(),
                exact: true,
            },
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
                failure
                    .summary
                    .contains("still registered to the previous binary (/old/csr-engine)"),
                "{prior:?}: {}",
                failure.summary
            );
            assert!(
                !failure.summary.contains("NOT registered"),
                "{prior:?}: it IS registered, just to the wrong binary"
            );
        }
    }

    /// We refused to remove because we could not snapshot it — the entry is
    /// still there, but we cannot name what it points at.
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
                .contains("still registered to the previous binary, not the new one"),
            "{}",
            failure.summary
        );
    }

    /// Removing a working registration and failing to re-add it is the worst
    /// outcome available, so it has to read differently from the others.
    #[test]
    fn test_mcp_registration_error_for_a_lost_entry() {
        let failure = mcp_registration_error(
            "/new/csr-engine",
            "boom",
            &PriorEntry::Lost("/old/csr-engine".to_string()),
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
        // Nothing is there, so a remove would only fail.
        assert!(
            !failure.detail.contains("claude mcp remove"),
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
                command: "/old/csr-engine".to_string(),
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
        assert!(!PriorEntry::Lost("/old".to_string()).still_registered());
        assert!(PriorEntry::Intact(None).still_registered());
        assert!(PriorEntry::Intact(Some("/old".to_string())).still_registered());
        assert!(PriorEntry::Restored {
            command: "/old".to_string(),
            exact: true
        }
        .still_registered());
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
