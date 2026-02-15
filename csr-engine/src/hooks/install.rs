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

/// Generate the hook configuration JSON.
fn generate_hook_config(binary_path: &str) -> serde_json::Value {
    serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "matcher": "startup|resume",
                "command": format!("{} hook session-start", binary_path)
            }],
            "SessionEnd": [{
                "command": format!("{} hook session-end", binary_path)
            }],
            "PreCompact": [{
                "command": format!("{} hook precompact", binary_path)
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
            // Replace entries for each hook type (SessionStart, etc.)
            // CSR owns these entries; other tools use different hook types
            hooks_obj[hook_type] = entries.clone();
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
    fn test_generate_hook_config() {
        let config = generate_hook_config("/usr/local/bin/csr-engine");
        let hooks = config.get("hooks").unwrap();

        let session_start = hooks.get("SessionStart").unwrap();
        assert!(session_start.is_array());
        let cmd = session_start[0]["command"].as_str().unwrap();
        assert!(cmd.contains("csr-engine hook session-start"));

        let session_end = hooks.get("SessionEnd").unwrap();
        let cmd = session_end[0]["command"].as_str().unwrap();
        assert!(cmd.contains("csr-engine hook session-end"));

        let precompact = hooks.get("PreCompact").unwrap();
        let cmd = precompact[0]["command"].as_str().unwrap();
        assert!(cmd.contains("csr-engine hook precompact"));
    }
}
