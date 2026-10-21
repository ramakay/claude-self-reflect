//! Hook installation — generates or applies Claude Code settings.json hook config.
//!
//! `csr-engine hook install` outputs the JSON config.
//! `csr-engine hook install --apply` auto-patches ~/.claude/settings.json.

use anyhow::Result;

use super::statusline_retire;

/// Handle the install subcommand.
pub fn handle(apply: bool) -> Result<()> {
    let binary_path = std::env::current_exe()?;
    let binary_str = shell_command_path(&binary_path);

    let config = generate_hook_config(&binary_str);

    if apply {
        let notes = apply_to_settings(&config)?;
        println!("CSR: Hooks installed to ~/.claude/settings.json");
        for note in notes {
            println!("{note}");
        }
        println!("{}", serde_json::to_string_pretty(&config)?);
    } else {
        println!("Add the following to your ~/.claude/settings.json:\n");
        println!("{}", serde_json::to_string_pretty(&config)?);
        println!("\nOr run with --apply to auto-patch settings.json.");
        if let Some(note) = statusline_preview() {
            println!("\n{note}");
        }
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
///
/// The result is then shell-quoted: an install under `/Users/me/CSR Tools/bin`
/// would otherwise produce a hook that runs `/Users/me/CSR`, and an apostrophe,
/// semicolon or `$(...)` in the path would change the command outright. Quoting
/// happens after the separator fix so a quoted Windows path keeps its slashes.
fn shell_command_path(binary: &std::path::Path) -> String {
    let rendered = binary.to_string_lossy();
    let normalized = if cfg!(windows) {
        rendered.replace('\\', "/")
    } else {
        rendered.into_owned()
    };
    crate::shell::shell_quote(&normalized)
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
                    "matcher": "startup|resume|compact|clear",
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
                "matcher": "Edit|Write|MultiEdit|NotebookEdit|Bash",
                "hooks": [{"type": "command", "command": format!("{} hook post-tool-use", binary_path)}]
            }],
            "UserPromptSubmit": [{
                "hooks": [{"type": "command", "command": format!("{} hook prompt-submit", binary_path)}]
            }]
        }
    })
}

/// Split a hook command into shell words, undoing the quoting `shell_quote` (and
/// hand-written quoting) produces. Returns None unless the whole command reads
/// as one simple command: an unterminated quote, an unquoted newline, `;`, `&`,
/// `|`, `(`, `)`, a backslash-newline continuation, or a backtick / `$(`
/// substitution (even inside double quotes) means a compound or unreadable
/// line, which the caller must treat as someone else's. `<` and `>` are
/// rejected only in the first two words, so `... hook stop 2>/dev/null` still
/// reads as a CSR invocation.
fn shell_words(command: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut chars = command.trim().chars().peekable();
    loop {
        while chars.next_if(|c| matches!(c, ' ' | '\t')).is_some() {}
        if chars.peek().is_none() {
            return Some(words);
        }
        let mut word = String::new();
        while let Some(c) = chars.next_if(|c| !matches!(c, ' ' | '\t')) {
            match c {
                '\'' => loop {
                    match chars.next()? {
                        '\'' => break,
                        other => word.push(other),
                    }
                },
                '"' => loop {
                    match chars.next()? {
                        '"' => break,
                        '`' => return None,
                        '$' if chars.peek() == Some(&'(') => return None,
                        '\\' => match chars.next()? {
                            escaped @ ('"' | '\\' | '$' | '`') => word.push(escaped),
                            other => {
                                word.push('\\');
                                word.push(other);
                            }
                        },
                        other => word.push(other),
                    }
                },
                '\\' => match chars.next()? {
                    '\n' | '\r' => return None,
                    escaped => word.push(escaped),
                },
                '\n' | '\r' | ';' | '&' | '|' | '(' | ')' | '`' => return None,
                '$' if chars.peek() == Some(&'(') => return None,
                '<' | '>' if words.len() < 2 => return None,
                other => word.push(other),
            }
        }
        words.push(word);
    }
}

/// True for a command that is a single invocation of a csr-engine binary's
/// `hook` subcommand, with the binary path quoted or not. Deliberately narrower than a substring
/// match: a user's `notify.sh` under a claude-self-reflect checkout, or
/// `echo csr-engine down`, is not a CSR hook.
fn is_csr_hook_command(command: &str) -> bool {
    let Some(words) = shell_words(command) else {
        return false;
    };
    let [binary, subcommand, ..] = words.as_slice() else {
        return false;
    };
    let binary = binary.rsplit('/').next().unwrap_or_default();
    matches!(binary, "csr-engine" | "csr-engine.exe") && subcommand == "hook"
}

/// Legacy CSR agent-type hooks carry no command, so they are recognised by the
/// prompt they shipped with.
fn is_csr_agent_hook(hook: &serde_json::Value) -> bool {
    hook.get("type").and_then(|t| t.as_str()) == Some("agent")
        && hook
            .get("prompt")
            .and_then(|p| p.as_str())
            .is_some_and(|prompt| {
                prompt.contains("CSR Episode Analyst") || prompt.contains("session_episode")
            })
}

/// A single nested hook CSR registered, judged narrowly enough to run over
/// event groups CSR never wrote to.
fn is_csr_hook(hook: &serde_json::Value) -> bool {
    hook.get("command")
        .and_then(|c| c.as_str())
        .is_some_and(is_csr_hook_command)
        || is_csr_agent_hook(hook)
}

/// Remove CSR's hooks from each entry of an event group CSR no longer
/// registers. Filters the nested hooks one by one so a user's command and the
/// entry's matcher survive next to CSR's, in either order; an entry goes only
/// when CSR emptied it. Returns true if CSR removed anything and left the group
/// empty.
fn sweep_retired_group(entries: &mut Vec<serde_json::Value>) -> bool {
    let before = entries.len();
    let mut swept = false;
    entries.retain_mut(|entry| {
        let Some(hooks) = entry.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
            return true;
        };
        let had = hooks.len();
        hooks.retain(|hook| !is_csr_hook(hook));
        if hooks.len() == had {
            return true;
        }
        swept = true;
        !hooks.is_empty()
    });
    swept && before > 0 && entries.is_empty()
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
///
/// Event groups the new config no longer registers (SubagentStop, retired) are
/// swept too: a CSR entry left there would keep spawning the binary on every
/// event. A group is deleted only when that sweep emptied it.
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

        if let Some(existing) = hooks_obj.as_object_mut() {
            existing.retain(|hook_type, group| {
                if new_hooks.contains_key(hook_type) {
                    return true;
                }
                let Some(entries) = group.as_array_mut() else {
                    return true;
                };
                !sweep_retired_group(entries)
            });
        }
    }
    Ok(())
}

/// The path of the user settings file under `home`.
fn settings_path_in(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".claude").join("settings.json")
}

/// The settings file hooks are installed into: `~/.claude/settings.json`.
pub fn default_settings_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|home| settings_path_in(&home))
}

/// What `--apply` would do to a retired CSR status line wrapper, for the dry run.
/// Reads only; an unreadable or non-JSON settings file says nothing here.
fn statusline_preview() -> Option<String> {
    let home = dirs::home_dir()?;
    let path = settings_path_in(&home);
    let mut settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    let outcome = statusline_retire::restore_in_place(&mut settings, &home);
    statusline_retire::report(&outcome, &path, true)
}

/// Whether the PostToolUse entries in `settings_path` run CSR's
/// `hook post-tool-use` for Bash calls: the matcher selects `Bash` by Claude Code's
/// rules (see `matcher_covers`: every tool, an exact name in a `|`/`,` list, or a regex). A
/// missing or unreadable file, or one without such an entry, is false: the
/// live hook is not recording Bash yet.
pub fn post_tool_use_records_bash(settings_path: &std::path::Path) -> bool {
    let Ok(text) = std::fs::read_to_string(settings_path) else {
        return false;
    };
    let Ok(settings) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    let Some(entries) = settings
        .pointer("/hooks/PostToolUse")
        .and_then(|v| v.as_array())
    else {
        return false;
    };
    entries.iter().any(|entry| {
        let matcher = entry.get("matcher").and_then(|m| m.as_str()).unwrap_or("");
        matcher_covers(matcher, "Bash")
            && entry
                .get("hooks")
                .and_then(|h| h.as_array())
                .is_some_and(|hooks| {
                    hooks.iter().any(|hook| {
                        hook.get("command")
                            .and_then(|c| c.as_str())
                            .is_some_and(is_csr_post_tool_use_command)
                    })
                })
    })
}

/// Does a hook `matcher` select `tool`? Claude Code's rules: `*`, empty or missing match
/// every tool. A matcher of only letters, digits, `_`, `-`, spaces, `,` and `|` is an
/// exact name, or a list of exact names split on `|` or `,` (spaces around a name are
/// ignored). Anything else is a JavaScript regex tested unanchored. Claude Code runs the
/// pattern as JavaScript, but this reads it with Rust's `regex`, which has no lookaround or
/// backreferences: such a pattern does not compile here and counts as not selecting the
/// tool, so the caller waits (the replay stays `pending`). That safe side is deliberate; no
/// second regex engine is added for it.
fn matcher_covers(matcher: &str, tool: &str) -> bool {
    if matcher.is_empty() || matcher == "*" {
        return true;
    }
    let plain = matcher
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ' ' | ',' | '|'));
    if plain {
        return matcher.split(['|', ',']).any(|name| name.trim() == tool);
    }
    // JavaScript-only syntax fails to compile here and reads as "does not select the tool".
    regex::Regex::new(matcher).is_ok_and(|re| re.is_match(tool))
}

/// `is_csr_hook_command` narrowed to the `post-tool-use` subcommand.
fn is_csr_post_tool_use_command(command: &str) -> bool {
    is_csr_hook_command(command)
        && shell_words(command)
            .is_some_and(|words| words.get(2).map(String::as_str) == Some("post-tool-use"))
}

/// Apply hook config to ~/.claude/settings.json.
/// Merges new hooks into existing config (does not clobber user hooks).
/// Uses atomic write (write to .tmp then rename) and creates a .bak backup.
///
/// The same read and write also puts a retired CSR status line wrapper's original
/// command back; the leftovers are deleted only once that write has succeeded.
/// Returns the lines to print about the status line.
fn apply_to_settings(config: &serde_json::Value) -> Result<Vec<String>> {
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    let settings_path = settings_path_in(&home);

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

    // Put the original status line command back if the retired wrapper is still set
    let outcome = statusline_retire::restore_in_place(&mut settings, &home);

    // Atomic write: write to temp file then rename
    let tmp_path = settings_path.with_extension("json.tmp");
    std::fs::write(&tmp_path, serde_json::to_string_pretty(&settings)?)?;
    std::fs::rename(&tmp_path, &settings_path)?;

    let mut notes: Vec<String> = statusline_retire::report(&outcome, &settings_path, false)
        .into_iter()
        .collect();
    if matches!(outcome, statusline_retire::Outcome::Restored(_)) {
        notes.extend(
            statusline_retire::delete_leftovers(&home)
                .into_iter()
                .map(|failure| format!("CSR: {failure}")),
        );
    }
    Ok(notes)
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
            // reach the shell as itself — which now means inside quotes.
            let path = r"/opt/we\ird/csr-engine";
            assert_eq!(
                shell_command_path(std::path::Path::new(path)),
                r"'/opt/we\ird/csr-engine'"
            );
        }
    }

    /// Claude Code runs each hook entry through a shell, so a path with a space
    /// or an apostrophe has to be quoted or every hook exits 127 — while setup
    /// still reports success.
    #[test]
    fn test_hook_commands_quote_awkward_paths_and_leave_plain_ones_bare() {
        let plain = generate_hook_config("/home/me/.local/bin/csr-engine");
        assert_eq!(
            plain["hooks"]["Stop"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap(),
            "/home/me/.local/bin/csr-engine hook stop",
            "an ordinary path must not grow quotes"
        );

        let spaced = generate_hook_config(&crate::shell::shell_quote(
            "/Users/me/CSR Tools/bin/csr-engine",
        ));
        assert_eq!(
            spaced["hooks"]["Stop"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap(),
            "'/Users/me/CSR Tools/bin/csr-engine' hook stop"
        );

        let quoted = shell_command_path(std::path::Path::new("/tmp/o'brien/CSR Tools/csr-engine"));
        let config = generate_hook_config(&quoted);
        assert_eq!(
            config["hooks"]["Stop"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap(),
            "'/tmp/o'\\''brien/CSR Tools/csr-engine' hook stop"
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

        assert!(
            hooks.get("SubagentStop").is_none(),
            "the SubagentStop hook is retired"
        );

        let post_tool_use = hooks.get("PostToolUse").unwrap();
        let cmd = post_tool_use[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("csr-engine hook post-tool-use"));
        assert_eq!(
            post_tool_use[0]["matcher"].as_str().unwrap(),
            "Edit|Write|MultiEdit|NotebookEdit|Bash"
        );
    }

    /// A settings.json written by an earlier release carries the old PostToolUse
    /// matcher. `hook install --apply` must replace that entry, not add a second
    /// one next to it (which would fire the hook twice on an Edit) and not leave
    /// the old matcher in place (which would never fire it for Bash). A user's
    /// own PostToolUse hook stays.
    #[test]
    fn test_merge_upgrades_the_old_post_tool_use_matcher_in_place() {
        let user = serde_json::json!({
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": "/Users/me/bin/audit-bash.sh"}]
        });
        let old_csr = serde_json::json!({
            "matcher": "Edit|Write|MultiEdit|NotebookEdit",
            "hooks": [{"type": "command", "command": "/usr/local/bin/csr-engine hook post-tool-use"}]
        });
        let mut settings = serde_json::json!({
            "hooks": {"PostToolUse": [old_csr, user.clone()]}
        });

        let new_config = generate_hook_config("/usr/local/bin/csr-engine");
        merge_hook_config(&mut settings, &new_config).unwrap();

        let entries = settings["hooks"]["PostToolUse"].as_array().unwrap();
        let csr: Vec<&serde_json::Value> = entries
            .iter()
            .filter(|e| {
                e["hooks"][0]["command"]
                    .as_str()
                    .unwrap()
                    .contains("csr-engine")
            })
            .collect();
        assert_eq!(
            csr.len(),
            1,
            "exactly one CSR PostToolUse entry: {entries:?}"
        );
        assert_eq!(
            csr[0]["matcher"].as_str().unwrap(),
            "Edit|Write|MultiEdit|NotebookEdit|Bash"
        );
        assert!(entries.contains(&user), "the user's own hook survives");
        assert_eq!(entries.len(), 2);

        // A second apply changes nothing.
        let once = settings.clone();
        merge_hook_config(&mut settings, &new_config).unwrap();
        assert_eq!(settings, once);
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
            "startup|resume|compact|clear"
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

    #[test]
    fn test_merge_migrates_the_old_session_start_matcher_to_include_clear() {
        let mut settings = serde_json::json!({
            "hooks": {"SessionStart": [
                {"hooks": [{"type": "command", "command": "node /Users/me/other-tool.js"}]},
                {"matcher": "startup|resume|compact",
                 "hooks": [{"type": "command", "command": "/usr/local/bin/csr-engine hook session-start"}]},
                {"matcher": "startup|resume",
                 "hooks": [{"type": "command", "command": "/usr/local/bin/csr-engine hook session-briefing", "async": true}]}
            ]}
        });
        merge_hook_config(
            &mut settings,
            &generate_hook_config("/usr/local/bin/csr-engine"),
        )
        .unwrap();
        let entries = settings["hooks"]["SessionStart"].as_array().unwrap();
        let matchers: Vec<_> = entries.iter().map(|e| e["matcher"].as_str()).collect();
        // The user's own hook stays; one session-start entry now covers clear; briefing is unchanged.
        assert_eq!(
            matchers,
            [
                None,
                Some("startup|resume|compact|clear"),
                Some("startup|resume")
            ]
        );
        assert!(!entries
            .iter()
            .any(|e| e["matcher"].as_str() == Some("startup|resume|compact")));
    }

    fn csr_subagent_stop_entry() -> serde_json::Value {
        serde_json::json!({
            "hooks": [{"type": "command", "command": "/usr/local/bin/csr-engine hook subagent-stop"}]
        })
    }

    fn user_subagent_stop_entry() -> serde_json::Value {
        serde_json::json!({
            "hooks": [{"type": "command", "command": "/Users/me/bin/notify-subagent.sh"}]
        })
    }

    #[test]
    fn test_generated_config_registers_no_subagent_stop() {
        let config = generate_hook_config("/usr/local/bin/csr-engine");
        assert!(config["hooks"].get("SubagentStop").is_none());
        assert!(!config.to_string().contains("subagent-stop"));
    }

    #[test]
    fn test_merge_drops_retired_csr_entry_but_keeps_the_users() {
        let mut settings = serde_json::json!({
            "hooks": {"SubagentStop": [csr_subagent_stop_entry(), user_subagent_stop_entry()]}
        });

        let new_config = generate_hook_config("/usr/local/bin/csr-engine");
        merge_hook_config(&mut settings, &new_config).unwrap();

        assert_eq!(
            settings["hooks"]["SubagentStop"],
            serde_json::json!([user_subagent_stop_entry()]),
            "only the user's own SubagentStop hook may remain"
        );
    }

    #[test]
    fn test_merge_removes_a_group_that_only_held_retired_csr_entries() {
        let mut settings = serde_json::json!({
            "hooks": {"SubagentStop": [csr_subagent_stop_entry()]}
        });

        let new_config = generate_hook_config("/usr/local/bin/csr-engine");
        merge_hook_config(&mut settings, &new_config).unwrap();

        assert!(settings["hooks"].get("SubagentStop").is_none());
        assert!(
            settings["hooks"].get("Stop").is_some(),
            "current hooks are still installed"
        );
    }

    #[test]
    fn test_merge_leaves_groups_it_did_not_empty_alone() {
        let mut settings = serde_json::json!({
            "hooks": {
                "Notification": [],
                "SubagentStart": [user_subagent_stop_entry()]
            },
            "model": "opus"
        });

        let new_config = generate_hook_config("/usr/local/bin/csr-engine");
        merge_hook_config(&mut settings, &new_config).unwrap();

        assert_eq!(settings["hooks"]["Notification"], serde_json::json!([]));
        assert_eq!(
            settings["hooks"]["SubagentStart"],
            serde_json::json!([user_subagent_stop_entry()])
        );
        assert_eq!(settings["model"], "opus");
    }

    fn hook_cmd(command: &str) -> serde_json::Value {
        serde_json::json!({"type": "command", "command": command})
    }

    fn merged(mut settings: serde_json::Value) -> serde_json::Value {
        let new_config = generate_hook_config("/usr/local/bin/csr-engine");
        merge_hook_config(&mut settings, &new_config).unwrap();
        settings
    }

    fn records_bash(post_tool_use: serde_json::Value) -> bool {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let settings = serde_json::json!({"hooks": {"PostToolUse": post_tool_use}});
        std::fs::write(&path, settings.to_string()).unwrap();
        post_tool_use_records_bash(&path)
    }

    #[test]
    fn test_post_tool_use_records_bash_reads_the_matcher() {
        let csr = hook_cmd("/usr/local/bin/csr-engine hook post-tool-use");
        let with = |matcher: &str| serde_json::json!([{"matcher": matcher, "hooks": [csr]}]);
        assert!(records_bash(with("Edit|Write|MultiEdit|NotebookEdit|Bash")));
        assert!(records_bash(with("Bash")));
        assert!(records_bash(with("*")));
        assert!(records_bash(with("")));
        assert!(records_bash(serde_json::json!([{"hooks": [csr]}])));
        assert!(!records_bash(with("Edit|Write|MultiEdit|NotebookEdit")));
        assert!(!records_bash(with("BashOutput")));
        // Bash is covered, but not by CSR's post-tool-use hook.
        assert!(!records_bash(serde_json::json!([
            {"matcher": "Bash", "hooks": [hook_cmd("/usr/local/bin/csr-engine hook stop")]},
            {"matcher": "Bash", "hooks": [hook_cmd("notify.sh post-tool-use")]},
        ])));
        // A real generated-and-merged config is recognised.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let settings = merged(serde_json::json!({}));
        std::fs::write(&path, settings.to_string()).unwrap();
        assert!(post_tool_use_records_bash(&path));
    }

    /// Claude Code's matcher rules: `*`/empty/missing match all; letters, digits, `_`, `-`,
    /// spaces, `,` and `|` only is a list of exact names; anything else is an unanchored
    /// JavaScript regex.
    #[test]
    fn test_post_tool_use_records_bash_follows_claude_codes_matcher_rules() {
        let csr = hook_cmd("/usr/local/bin/csr-engine hook post-tool-use");
        let with = |matcher: &str| serde_json::json!([{"matcher": matcher, "hooks": [csr]}]);
        for yes in [
            "Bash",
            "Edit|Write|Bash",
            "Edit, Bash",
            "Edit,Bash",
            " Bash ",
            "*",
            "",
            ".*",
            "^Bash$",
            "B.sh",
            "Bash.*",
            "(Edit|Bash)",
        ] {
            assert!(records_bash(with(yes)), "matcher {yes:?} should cover Bash");
        }
        assert!(records_bash(serde_json::json!([{"hooks": [csr]}])));
        for no in [
            "Bas",
            "bash",
            "Edit|Write",
            "^Edit$",
            "Edit, Write",
            "BashOutput",
            "([unclosed",
            "(?<=x)Bash",
        ] {
            assert!(
                !records_bash(with(no)),
                "matcher {no:?} must not cover Bash"
            );
        }
    }

    #[test]
    fn test_post_tool_use_records_bash_is_false_without_a_readable_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!post_tool_use_records_bash(&dir.path().join("none.json")));
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{not json").unwrap();
        assert!(!post_tool_use_records_bash(&bad));
    }

    /// The sweep covers events CSR never installed, so it must only match a
    /// real `csr-engine hook ...` invocation: a user's script that merely lives
    /// under a `claude-self-reflect` checkout or mentions `csr-engine` is theirs.
    #[test]
    fn test_sweep_keeps_user_commands_that_only_mention_csr() {
        let user_entries = serde_json::json!([
            {"hooks": [hook_cmd("bash /Users/x/projects/claude-self-reflect/scripts/notify.sh")]},
            {"hooks": [hook_cmd("echo csr-engine down")]},
            {"hooks": [hook_cmd("/opt/csr-engine-tools/bin/notify hook subagent-stop")]},
            {"hooks": [hook_cmd("/usr/local/bin/csr-engine status")]},
            {"hooks": [hook_cmd("/Users/x/claude-self-reflect/target/release/csr-engine")]},
            {"hooks": [hook_cmd("'unterminated /usr/bin/csr-engine hook stop")]},
        ]);
        let settings = merged(serde_json::json!({"hooks": {"Notification": user_entries}}));
        assert_eq!(settings["hooks"]["Notification"], user_entries);
    }

    /// A CSR-looking prefix does not make a compound or unreadable line CSR's:
    /// the user's command rides along, so the whole entry is kept.
    #[test]
    fn test_sweep_keeps_compound_and_unreadable_commands() {
        let kept = [
            "/usr/local/bin/csr-engine hook subagent-stop; notify-send done",
            "/usr/local/bin/csr-engine hook subagent-stop && notify-send done",
            "/usr/local/bin/csr-engine hook subagent-stop || true",
            "/usr/local/bin/csr-engine hook subagent-stop | tee /tmp/log",
            "/usr/local/bin/csr-engine hook subagent-stop &",
            "/usr/local/bin/csr-engine hook subagent-stop 2>&1",
            "/usr/local/bin/csr-engine hook subagent-stop\nrm -rf /tmp/x",
            "/usr/local/bin/csr-engine hook subagent-stop `whoami`",
            "/usr/local/bin/csr-engine hook subagent-stop $(whoami)",
            "/usr/local/bin/csr-engine hook subagent-stop \"$(whoami)\"",
            "/usr/local/bin/csr-engine hook subagent-stop \"`whoami`\"",
            "/usr/local/bin/csr-engine hook subagent-stop 'unterminated",
            "/usr/local/bin/csr-engine hook subagent-stop \"unterminated",
            "/usr/local/bin/csr-engine hook subagent-stop \\\n  --extra",
            "(/usr/local/bin/csr-engine hook subagent-stop)",
            "/usr/local/bin/csr-engine > /tmp/out hook subagent-stop",
        ];
        for command in kept {
            let entries = serde_json::json!([{"hooks": [hook_cmd(command)]}]);
            let settings = merged(serde_json::json!({"hooks": {"SubagentStop": entries}}));
            assert_eq!(
                settings["hooks"]["SubagentStop"], entries,
                "{command:?} must be kept"
            );
        }
    }

    #[test]
    fn test_sweep_still_removes_a_plain_invocation_with_quoted_operators_and_redirects() {
        for command in [
            "/usr/local/bin/csr-engine hook subagent-stop 2>/dev/null",
            "/usr/local/bin/csr-engine hook subagent-stop 'a;b' \"c|d\"",
            "  /usr/local/bin/csr-engine   hook\tsubagent-stop  ",
        ] {
            let settings = merged(serde_json::json!({
                "hooks": {"SubagentStop": [{"hooks": [hook_cmd(command)]}]}
            }));
            assert!(
                settings["hooks"].get("SubagentStop").is_none(),
                "{command:?} should have been swept"
            );
        }
    }

    #[test]
    fn test_sweep_removes_csr_invocations_however_the_path_is_written() {
        for command in [
            "/usr/local/bin/csr-engine hook subagent-stop",
            "'/Users/me/CSR Tools/bin/csr-engine' hook subagent-stop",
            "\"/Users/me/CSR Tools/bin/csr-engine\" hook subagent-stop",
            "'/tmp/o'\\''brien/csr-engine' hook subagent-stop",
            "csr-engine hook subagent-stop",
            "C:/Users/me/bin/csr-engine.exe hook subagent-stop",
        ] {
            let settings = merged(serde_json::json!({
                "hooks": {"SubagentStop": [{"hooks": [hook_cmd(command)]}]}
            }));
            assert!(
                settings["hooks"].get("SubagentStop").is_none(),
                "{command} should have been swept"
            );
        }
    }

    #[test]
    fn test_sweep_removes_legacy_csr_agent_hooks_from_unregistered_groups() {
        let settings = merged(serde_json::json!({
            "hooks": {"SubagentStop": [{"hooks": [
                {"type": "agent", "prompt": "You are CSR Episode Analyst. Summarize..."}
            ]}]}
        }));
        assert!(settings["hooks"].get("SubagentStop").is_none());
    }

    /// An entry holds several hooks and the matcher belongs to the entry: the
    /// sweep removes CSR's hook alone, in either order, and drops the entry
    /// only when nothing is left in it.
    #[test]
    fn test_sweep_filters_individual_hooks_in_mixed_entries() {
        let csr = hook_cmd("/usr/local/bin/csr-engine hook subagent-stop");
        let user = hook_cmd("/Users/me/bin/notify-subagent.sh");
        for hooks in [
            serde_json::json!([csr.clone(), user.clone()]),
            serde_json::json!([user.clone(), csr.clone()]),
        ] {
            let settings = merged(serde_json::json!({
                "hooks": {"SubagentStop": [{"matcher": "general-purpose", "hooks": hooks}]}
            }));
            assert_eq!(
                settings["hooks"]["SubagentStop"],
                serde_json::json!([{"matcher": "general-purpose", "hooks": [user.clone()]}]),
                "matcher and the user's command survive, CSR's goes"
            );
        }

        let settings = merged(serde_json::json!({
            "hooks": {"SubagentStop": [
                {"matcher": "x", "hooks": [csr.clone(), csr.clone()]},
                {"hooks": [user.clone()]}
            ]}
        }));
        assert_eq!(
            settings["hooks"]["SubagentStop"],
            serde_json::json!([{"hooks": [user]}]),
            "an entry CSR emptied is removed, the rest stay"
        );
    }

    #[test]
    fn test_sweep_leaves_entries_without_a_hooks_array_and_empty_ones_alone() {
        let odd = serde_json::json!([
            {"matcher": "x"},
            {"hooks": []},
            "not an entry"
        ]);
        let settings = merged(serde_json::json!({"hooks": {"SubagentStop": odd}}));
        assert_eq!(settings["hooks"]["SubagentStop"], odd);
    }

    /// The installers tell a user whose hooks point at a different copy of
    /// csr-engine to re-run `<new binary> setup`, and that advice is only true
    /// because the merge evicts CSR entries by command content rather than by
    /// path. If eviction ever became path-sensitive, every upgrade would leave
    /// the old absolute path installed alongside the new one and the printed
    /// remedy would be a lie.
    #[test]
    fn test_merge_repoints_hooks_at_a_new_absolute_path() {
        let mut settings: serde_json::Value = serde_json::json!({
            "hooks": {
                "Stop": [
                    {"hooks": [{"type": "command", "command": "/usr/local/bin/csr-engine hook stop"}]}
                ]
            }
        });

        let new_config = generate_hook_config("/home/me/.local/bin/csr-engine");
        merge_hook_config(&mut settings, &new_config).unwrap();

        let stop = settings["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1, "repointed, not duplicated");
        assert_eq!(
            stop[0]["hooks"][0]["command"].as_str().unwrap(),
            "/home/me/.local/bin/csr-engine hook stop",
            "the old absolute path must not survive"
        );
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
