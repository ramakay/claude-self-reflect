use super::*;
use serde_json::json;

#[test]
fn a_hook_command_names_its_binary_quoted_or_not() {
    assert_eq!(
        hook_binary("/Users/me/.local/bin/csr-engine hook stop"),
        Some(PathBuf::from("/Users/me/.local/bin/csr-engine"))
    );
    assert_eq!(
        hook_binary("\"/Users/me/My Tools/csr-engine\" hook stop 2>/dev/null"),
        Some(PathBuf::from("/Users/me/My Tools/csr-engine"))
    );
    assert_eq!(
        hook_binary("'/opt/csr/csr-engine' hook session-start"),
        Some(PathBuf::from("/opt/csr/csr-engine"))
    );
}

#[test]
fn only_an_absolute_csr_engine_hook_command_counts_as_a_binary() {
    for not_ours in [
        "csr-engine hook stop",
        "/usr/bin/other hook stop",
        "/opt/csr/csr-engine status",
        "/opt/csr/csr-engine",
        "\"/opt/csr/csr-engine hook stop",
        "",
    ] {
        assert_eq!(hook_binary(not_ours), None, "{not_ours:?}");
    }
}

#[test]
fn settings_name_each_hook_binary_and_skip_foreign_hooks() {
    let settings = json!({"hooks": {
        "Stop": [{"hooks": [
            {"type": "command", "command": "/a/csr-engine hook stop"},
            {"type": "command", "command": "echo mine"}
        ]}],
        "SessionEnd": [{"hooks": [{"type": "command", "command": "/b/csr-engine hook session-end"}]}],
        "Notification": [{"hooks": [{"type": "agent", "prompt": "x"}]}]
    }});
    let mut found = settings_binaries(&settings);
    found.sort();
    assert_eq!(
        found,
        vec![
            PathBuf::from("/a/csr-engine"),
            PathBuf::from("/b/csr-engine")
        ]
    );
    assert!(settings_binaries(&json!({"model": "opus"})).is_empty());
}

#[test]
fn a_plist_names_its_program_with_entities_undone() {
    let plist = "<plist><dict><key>Label</key><string>x</string>\n<key>ProgramArguments</key>\n\
                 <array>\n<string>/Users/me/R&amp;D/csr-engine</string>\n<string>daemon</string>\n\
                 </array></dict></plist>";
    assert_eq!(
        plist_program(plist).as_deref(),
        Some("/Users/me/R&D/csr-engine")
    );
    assert_eq!(plist_program("<plist><dict></dict></plist>"), None);
    assert_eq!(plist_program("<key>ProgramArguments</key><array>"), None);
}

#[test]
fn claude_json_names_the_csr_engine_of_user_and_project_entries_only() {
    let config = json!({
        "mcpServers": {
            "claude-self-reflect": {"command": "/u/csr-engine"},
            "other": {"command": "/x/csr-engine"}
        },
        "projects": {
            "/p/one": {"mcpServers": {"claude-self-reflect": {"command": "/p/csr-engine"}}},
            "/p/two": {"mcpServers": {"claude-self-reflect": {"command": "/p/run-mcp.sh"}}},
            "/p/three": {}
        }
    });
    let mut found = claude_json_binaries(&config);
    found.sort();
    assert_eq!(
        found,
        vec![
            PathBuf::from("/p/csr-engine"),
            PathBuf::from("/u/csr-engine")
        ]
    );
}

#[test]
fn local_entries_list_every_project_with_a_server_of_that_name() {
    let config = json!({"projects": {
        "/p/one": {"mcpServers": {"claude-self-reflect": {
            "command": "/p/mcp-server/run-mcp.sh", "args": ["--flag"]}}},
        "/p/two": {"mcpServers": {"other": {"command": "x"}}},
        "/p/three": {"allowedTools": []}
    }});
    assert_eq!(
        local_entries(&config),
        vec![(
            "/p/one".to_string(),
            "/p/mcp-server/run-mcp.sh --flag".to_string()
        )]
    );
    assert!(local_entries(&json!({"mcpServers": {}})).is_empty());
    assert!(has_local_entry(&config, "/p/one"));
    assert!(!has_local_entry(&config, "/p/two"));
}

#[test]
fn npm_ls_json_is_read_for_the_package_and_not_the_exit_code() {
    let listed = r#"{"name":"lib","dependencies":{"claude-self-reflect":{"version":"9.5.8"}}}"#;
    assert_eq!(npm_version(listed).as_deref(), Some("9.5.8"));
    assert_eq!(
        npm_version(r#"{"dependencies":{"claude-self-reflect":{}}}"#).as_deref(),
        Some("unknown version")
    );
    assert_eq!(npm_version("{}"), None);
    assert_eq!(
        npm_version(r#"{"dependencies":{"other":{"version":"1"}}}"#),
        None
    );
    assert_eq!(npm_version("npm ERR! not json"), None);
    assert_eq!(npm_version(""), None);
}

#[test]
fn the_plugin_cache_goes_only_when_the_plugin_is_gone_and_a_link_loses_only_itself() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let cache = home.join(".claude/plugins/cache/claude-self-reflect");
    std::fs::create_dir_all(cache.join("csr-companion/10.1.0")).unwrap();
    std::fs::write(cache.join("csr-companion/10.1.0/plugin.json"), "{}").unwrap();

    let mut out = Vec::new();
    assert!(plugin_cache_step(&home, false, true, &mut out));
    assert!(out[0].contains("would delete") && cache.exists(), "{out:?}");

    let mut out = Vec::new();
    assert!(plugin_cache_step(&home, true, false, &mut out));
    assert!(out[0].contains("kept") && cache.exists(), "{out:?}");

    let mut out = Vec::new();
    assert!(plugin_cache_step(&home, true, true, &mut out));
    assert!(out[0].contains("removed") && !cache.exists(), "{out:?}");
    assert!(home.join(".claude/plugins/cache").exists());

    let mut out = Vec::new();
    assert!(plugin_cache_step(&home, true, true, &mut out));
    assert!(out[0].contains("does not exist"), "{out:?}");

    let target = root.path().join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("keep.txt"), "mine").unwrap();
    symlink(&target, &cache).unwrap();
    let mut out = Vec::new();
    assert!(plugin_cache_step(&home, true, true, &mut out));
    assert!(std::fs::symlink_metadata(&cache).is_err(), "{out:?}");
    assert!(target.join("keep.txt").exists());
}
