//! Hook installation — generates or applies Claude Code settings.json hook config.
//!
//! `csr-engine hook install` outputs the JSON config.
//! `csr-engine hook install --apply` auto-patches ~/.claude/settings.json.

use anyhow::Result;

/// Handle the install subcommand.
pub fn handle(apply: bool) -> Result<()> {
    let binary_path = std::env::current_exe()?;
    let binary_str = shell_command_path(&binary_path);

    let config = generate_hook_config(&binary_str);

    if apply {
        apply_to_settings(&config)?;
        println!("CSR: Hooks installed to ~/.claude/settings.json");
        println!("{}", serde_json::to_string_pretty(&config)?);
    } else {
        println!("Add the following to your ~/.claude/settings.json:\n");
        println!("{}", serde_json::to_string_pretty(&config)?);
        println!("\nOr run with --apply to auto-patch settings.json.");
    }

    Ok(())
}

/// Render the binary path for embedding in a hook command string.
///
/// Every hook entry is a single command string, which Claude Code runs through a
/// POSIX shell — and there `\` is an escape character, not a separator. A Windows
/// path is therefore delivered to the shell as `D:pathtocsr-engine.exe`, which
/// exits 127 with "command not found" before the process starts, so the failure
/// leaves no trace in hook-timing.log either. Forward slashes are accepted both by
/// the shell and by Windows itself. On Unix the separators are left alone, since a
/// backslash there is a legal filename character.
fn shell_command_path(binary: &std::path::Path) -> String {
    let rendered = binary.to_string_lossy();
    if cfg!(windows) {
        rendered.replace('\\', "/")
    } else {
        rendered.into_owned()
    }
}

/// Generate the hook configuration JSON (public for testing).
pub fn generate_hook_config_for_test(binary_path: &str) -> serde_json::Value {
    generate_hook_config(binary_path)
}

/// Generate the hook configuration JSON.
fn generate_hook_config(binary_path: &str) -> serde_json::Value {
    serde_json::json!({
        "hooks": {
            "SessionStart": [
                {
                    "matcher": "startup|resume|compact",
                    "hooks": [{"type": "command", "command": format!("{} hook session-start", binary_path)}]
                },
                {
                    "matcher": "startup|resume",
                    "hooks": [{
                        "type": "command",
                        "command": format!("{} hook session-briefing", binary_path),
                        "async": true,
                        "timeout": 150,
                        "statusMessage": "CSR generating session briefing (async)..."
                    }]
                }
            ],
            "SessionEnd": [{
                "hooks": [{"type": "command", "command": format!("{} hook session-end", binary_path)}]
            }],
            "PreCompact": [{
                "hooks": [{"type": "command", "command": format!("{} hook precompact", binary_path)}]
            }],
            "Stop": [{
                "hooks": [{"type": "command", "command": format!("{} hook stop", binary_path)}]
            }],
            "PostToolUse": [{
                "matcher": "Edit|Write|MultiEdit|NotebookEdit",
                "hooks": [{"type": "command", "command": format!("{} hook post-tool-use", binary_path)}]
            }],
            "UserPromptSubmit": [{
                "hooks": [{"type": "command", "command": format!("{} hook prompt-submit", binary_path)}]
            }]
        }
    })
}

/// Returns true if a settings hook entry was registered by CSR — covers both
/// command-type hooks (matched by command path) and agent-type hooks (matched by
/// prompt content). Used by merge_hook_config to evict stale CSR entries before
/// appending fresh ones. Agent hooks lack a `command` field so the command check
/// alone misses them — a real bug we hit shipping v9.2 (stale agent hook left
/// behind, kept firing ToolUseContext errors).
fn is_csr_entry(entry: &serde_json::Value) -> bool {
    let first_hook = entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .and_then(|arr| arr.first());

    let is_csr_command = first_hook
        .and_then(|h| h.get("command"))
        .and_then(|c| c.as_str())
        .is_some_and(|cmd| cmd.contains("csr-engine") || cmd.contains("claude-self-reflect"));

    let is_csr_agent = first_hook
        .and_then(|h| {
            if h.get("type").and_then(|t| t.as_str()) == Some("agent") {
                h.get("prompt").and_then(|p| p.as_str())
            } else {
                None
            }
        })
        .is_some_and(|prompt| {
            prompt.contains("CSR Episode Analyst") || prompt.contains("session_episode")
        });

    is_csr_command || is_csr_agent
}

/// Merge CSR hook config into existing settings JSON. Removes stale CSR entries,
/// appends fresh ones. Preserves non-CSR hooks (GSD, Moshi, etc).
fn merge_hook_config(
    settings: &mut serde_json::Value,
    config: &serde_json::Value,
) -> anyhow::Result<()> {
    if let Some(new_hooks) = config.get("hooks").and_then(|h| h.as_object()) {
        let hooks_obj = settings
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("settings.json is not an object"))?
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));

        for (hook_type, entries) in new_hooks {
            if let Some(existing_arr) = hooks_obj.get_mut(hook_type).and_then(|v| v.as_array_mut())
            {
                existing_arr.retain(|entry| !is_csr_entry(entry));
                if let Some(new_arr) = entries.as_array() {
                    existing_arr.extend(new_arr.iter().cloned());
                }
            } else {
                hooks_obj[hook_type] = entries.clone();
            }
        }
    }
    Ok(())
}

/// Apply hook config to ~/.claude/settings.json.
/// Merges new hooks into existing config (does not clobber user hooks).
/// Uses atomic write (write to .tmp then rename) and creates a .bak backup.
fn apply_to_settings(config: &serde_json::Value) -> Result<()> {
    let settings_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?
        .join(".claude")
        .join("settings.json");

    // Ensure parent directory exists
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Read existing settings or create new
    let mut settings: serde_json::Value = if settings_path.exists() {
        // Create backup before modification
        let backup_path = settings_path.with_extension("json.bak");
        std::fs::copy(&settings_path, &backup_path)?;

        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content)?
    } else {
        serde_json::json!({})
    };

    // Deep-merge hooks: add CSR hooks without clobbering existing user hooks
    merge_hook_config(&mut settings, config)?;

    // Atomic write: write to temp file then rename
    let tmp_path = settings_path.with_extension("json.tmp");
    std::fs::write(&tmp_path, serde_json::to_string_pretty(&settings)?)?;
    std::fs::rename(&tmp_path, &settings_path)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retain_removes_python_csr_hooks() {
        // Simulate existing settings with Python CSR hooks + a non-CSR hook
        let mut settings: serde_json::Value = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {
                        "hooks": [{"type": "command", "command": "node \"/Users/me/.claude/hooks/gsd-check-update.js\""}]
                    },
                    {
                        "hooks": [{"type": "command", "command": "python /Users/me/projects/claude-self-reflect/src/runtime/hooks/session_start_hook.py"}],
                        "matcher": "startup|resume"
                    }
                ],
                "SessionEnd": [
                    {
                        "hooks": [{"type": "command", "command": "/Users/me/projects/claude-self-reflect/src/runtime/hooks/session_end_hook.py"}]
                    }
                ],
                "PreCompact": [
                    {
                        "hooks": [{"type": "command", "command": "/Users/me/projects/claude-self-reflect/src/runtime/precompact-hook.sh"}]
                    }
                ]
            }
        });

        let new_config = generate_hook_config("/usr/local/bin/csr-engine");
        merge_hook_config(&mut settings, &new_config).unwrap();

        // GSD hook should survive
        let session_start = settings["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(session_start.len(), 3); // GSD + CSR command + CSR agent
        let gsd_cmd = session_start[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(
            gsd_cmd.contains("gsd-check-update"),
            "GSD hook should survive"
        );
        let csr_cmd = session_start[1]["hooks"][0]["command"].as_str().unwrap();
        assert!(
            csr_cmd.contains("csr-engine"),
            "New CSR hook should be added"
        );

        // Python hooks should be replaced with Rust hooks
        let session_end = settings["hooks"]["SessionEnd"].as_array().unwrap();
        assert_eq!(session_end.len(), 1); // Only new CSR, Python removed
        let cmd = session_end[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("csr-engine"), "Python hook should be replaced");
        assert!(
            !cmd.contains("session_end_hook.py"),
            "Python hook must be gone"
        );

        let precompact = settings["hooks"]["PreCompact"].as_array().unwrap();
        assert_eq!(precompact.len(), 1);
        let cmd = precompact[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(
            cmd.contains("csr-engine"),
            "Python precompact should be replaced"
        );
    }

    #[test]
    fn test_shell_command_path_survives_the_shell() {
        #[cfg(windows)]
        {
            let rendered = shell_command_path(std::path::Path::new(
                r"D:\Projects\claude-self-reflect\csr-engine.exe",
            ));
            assert_eq!(rendered, "D:/Projects/claude-self-reflect/csr-engine.exe");
            assert!(
                !rendered.contains('\\'),
                "a surviving backslash is eaten by the shell and the hook exits 127"
            );
        }
        #[cfg(unix)]
        {
            // A backslash is a legal character in a POSIX filename, so it must
            // reach the shell untouched.
            let path = r"/opt/we\ird/csr-engine";
            assert_eq!(shell_command_path(std::path::Path::new(path)), path);
        }
    }

    #[test]
    fn test_generate_hook_config() {
        let config = generate_hook_config("/usr/local/bin/csr-engine");
        let hooks = config.get("hooks").unwrap();

        let session_start = hooks.get("SessionStart").unwrap();
        assert!(session_start.is_array());
        let cmd = session_start[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("csr-engine hook session-start"));

        let session_end = hooks.get("SessionEnd").unwrap();
        let cmd = session_end[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("csr-engine hook session-end"));

        let precompact = hooks.get("PreCompact").unwrap();
        let cmd = precompact[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("csr-engine hook precompact"));

        let stop = hooks.get("Stop").unwrap();
        let cmd = stop[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("csr-engine hook stop"));

        let post_tool_use = hooks.get("PostToolUse").unwrap();
        let cmd = post_tool_use[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("csr-engine hook post-tool-use"));
        assert_eq!(
            post_tool_use[0]["matcher"].as_str().unwrap(),
            "Edit|Write|MultiEdit|NotebookEdit"
        );
    }

    #[test]
    fn test_generate_hook_config_has_async_briefing_hook() {
        let config = generate_hook_config("/usr/local/bin/csr-engine");
        let hooks = config.get("hooks").unwrap();

        let session_start = hooks.get("SessionStart").unwrap().as_array().unwrap();
        assert_eq!(
            session_start.len(),
            2,
            "SessionStart should have fast sync hook + async briefing hook"
        );

        // First: existing fast sync command hook
        let fast_hook = &session_start[0];
        assert_eq!(fast_hook["hooks"][0]["type"].as_str().unwrap(), "command");
        assert!(fast_hook["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("session-start"));
        assert_eq!(
            fast_hook["matcher"].as_str().unwrap(),
            "startup|resume|compact"
        );

        // Second: async command hook invoking claude -p for briefing
        let briefing_hook = &session_start[1];
        assert_eq!(
            briefing_hook["hooks"][0]["type"].as_str().unwrap(),
            "command"
        );
        assert!(briefing_hook["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("session-briefing"));
        assert!(briefing_hook["hooks"][0]["async"].as_bool().unwrap());
        assert_eq!(briefing_hook["hooks"][0]["timeout"].as_u64().unwrap(), 150);
        // Briefing only on startup|resume (not compact — compact doesn't need fresh briefing)
        assert_eq!(briefing_hook["matcher"].as_str().unwrap(), "startup|resume");
    }

    /// Regression test for the v9.2 agent-hook leak.
    ///
    /// We initially shipped v9.2 with an `agent`-type SessionStart hook. Agent hooks
    /// only work for tool events (PreToolUse/PostToolUse/PermissionRequest), so it
    /// fired `ToolUseContext is required for agent hooks` on every session start.
    ///
    /// We replaced it with an async command hook, but the original install merge logic
    /// matched CSR entries by `command` field only. Agent hooks have a `prompt` field
    /// instead — so re-running `hook install --apply` left the broken agent hook in
    /// settings.json alongside the new command hook. The error kept firing.
    ///
    /// This test verifies that agent hooks containing the CSR Episode Analyst prompt
    /// are correctly identified as CSR-owned and evicted during merge.
    #[test]
    fn test_merge_removes_stale_csr_agent_hook() {
        let mut settings: serde_json::Value = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {
                        "hooks": [{
                            "type": "agent",
                            "prompt": "You are CSR Episode Analyst. Search session_episode reflections...",
                            "model": "claude-haiku-4-5-20251001",
                            "async": true
                        }],
                        "matcher": "startup|resume"
                    },
                    {
                        "hooks": [{"type": "command", "command": "node /Users/me/other-tool.js"}]
                    }
                ]
            }
        });

        let new_config = generate_hook_config("/usr/local/bin/csr-engine");
        merge_hook_config(&mut settings, &new_config).unwrap();

        let session_start = settings["hooks"]["SessionStart"].as_array().unwrap();

        // No stale agent hook should remain
        let has_stale_agent = session_start.iter().any(|e| {
            let h = &e["hooks"][0];
            h["type"].as_str() == Some("agent")
                && h["prompt"]
                    .as_str()
                    .is_some_and(|p| p.contains("CSR Episode Analyst"))
        });
        assert!(
            !has_stale_agent,
            "Stale CSR agent hook should be evicted during merge"
        );

        // Non-CSR command hook should survive
        let has_other_tool = session_start.iter().any(|e| {
            e["hooks"][0]["command"]
                .as_str()
                .is_some_and(|c| c.contains("other-tool.js"))
        });
        assert!(has_other_tool, "Non-CSR hook should be preserved");
    }
}
