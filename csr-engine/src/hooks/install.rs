//! Hook installation — generates or applies Claude Code settings.json hook config.
//!
//! `csr-engine hook install` outputs the JSON config.
//! `csr-engine hook install --apply` auto-patches ~/.claude/settings.json.

use anyhow::Result;

/// Handle the install subcommand.
pub fn handle(apply: bool) -> Result<()> {
    let binary_path = std::env::current_exe()?;
    let binary_str = binary_path.to_string_lossy();

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
    if let Some(new_hooks) = config.get("hooks").and_then(|h| h.as_object()) {
        let hooks_obj = settings
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("settings.json is not an object"))?
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));

        for (hook_type, entries) in new_hooks {
            // Merge CSR entries: remove existing CSR entries, then append new ones.
            // Preserves entries from other tools under the same hook type.
            if let Some(existing_arr) = hooks_obj.get_mut(hook_type).and_then(|v| v.as_array_mut())
            {
                // Remove existing CSR entries — both Rust ("csr-engine") and Python
                // ("claude-self-reflect" hooks). Both live under the claude-self-reflect
                // project path, so matching that catches legacy Python hooks too.
                existing_arr.retain(|entry| {
                    let is_csr = entry
                        .get("hooks")
                        .and_then(|h| h.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|h| h.get("command"))
                        .and_then(|c| c.as_str())
                        .is_some_and(|cmd| {
                            cmd.contains("csr-engine") || cmd.contains("claude-self-reflect")
                        });
                    !is_csr
                });
                // Append new CSR entries
                if let Some(new_arr) = entries.as_array() {
                    existing_arr.extend(new_arr.iter().cloned());
                }
            } else {
                hooks_obj[hook_type] = entries.clone();
            }
        }
    }

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

        // Apply merge logic (same as apply_to_settings but inline)
        if let Some(new_hooks) = new_config.get("hooks").and_then(|h| h.as_object()) {
            let hooks_obj = settings.get_mut("hooks").unwrap();
            for (hook_type, entries) in new_hooks {
                if let Some(existing_arr) =
                    hooks_obj.get_mut(hook_type).and_then(|v| v.as_array_mut())
                {
                    existing_arr.retain(|entry| {
                        let is_csr = entry
                            .get("hooks")
                            .and_then(|h| h.as_array())
                            .and_then(|arr| arr.first())
                            .and_then(|h| h.get("command"))
                            .and_then(|c| c.as_str())
                            .is_some_and(|cmd| {
                                cmd.contains("csr-engine") || cmd.contains("claude-self-reflect")
                            });
                        !is_csr
                    });
                    if let Some(new_arr) = entries.as_array() {
                        existing_arr.extend(new_arr.iter().cloned());
                    }
                } else {
                    hooks_obj[hook_type] = entries.clone();
                }
            }
        }

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
        assert_eq!(briefing_hook["hooks"][0]["async"].as_bool().unwrap(), true);
        assert_eq!(briefing_hook["hooks"][0]["timeout"].as_u64().unwrap(), 150);
        // Briefing only on startup|resume (not compact — compact doesn't need fresh briefing)
        assert_eq!(briefing_hook["matcher"].as_str().unwrap(), "startup|resume");
    }
}
