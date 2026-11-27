//! What the generated launchd agent says about the daemon's scheduling.

use super::*;
use std::path::Path;

fn agent_spec(home: &Path) -> DaemonAgentSpec {
    DaemonAgentSpec {
        engine: "/opt/csr/bin/csr-engine".into(),
        home: home.to_path_buf(),
        db_path: home.join(".claude-self-reflect/csr-engine.db"),
        projects_dir: home.join(".claude/projects"),
        claude_dir: Some("/opt/claude/bin".into()),
        env: Default::default(),
        lock_holder: None,
    }
}

#[test]
fn the_daemon_plist_escapes_xml_in_paths() {
    let mut spec = agent_spec(Path::new("/Users/a&b"));
    spec.engine = "/Users/a&b/<bin>/csr-engine".into();
    let text = render_daemon_plist(&spec);
    assert!(
        text.contains("/Users/a&amp;b/&lt;bin&gt;/csr-engine"),
        "{text}"
    );
    assert!(!text.contains("a&b"), "{text}");
}

/// Under `ProcessType Standard` launchd holds every daemon thread at priority
/// 20 whatever its QoS class, so a prompt render ran at background priority
/// beside every interactive process. `Interactive` lets the class decide: the
/// render raises itself to user-initiated (37) and the runtime threads stay at
/// utility (20, what Standard gave them). `Adaptive` and `Background` throttle
/// the whole job (measured on the Studio, 2026-11-16).
#[test]
fn the_daemon_plist_lets_thread_qos_lift_the_prompt_render() {
    let text = render_daemon_plist(&agent_spec(Path::new("/Users/someone")));
    assert!(
        text.contains("<key>ProcessType</key>\n  <string>Interactive</string>"),
        "{text}"
    );
    for clamped in ["Standard", "Adaptive", "Background"] {
        assert!(
            !text.contains(&format!("<string>{clamped}</string>")),
            "{text}"
        );
    }
    assert!(
        text.contains("<key>Nice</key>\n  <integer>10</integer>"),
        "{text}"
    );
}

/// launchd sends SIGKILL `ExitTimeOut` seconds after SIGTERM. Without the key it used 5 s
/// (`launchctl print`: `exit timeout = 5`), shorter than a graceful stop that joins every loop
/// and flushes the index, so a stop could end in SIGKILL with nothing logged.
#[test]
fn the_daemon_plist_gives_the_graceful_stop_thirty_seconds() {
    let text = render_daemon_plist(&agent_spec(Path::new("/Users/someone")));
    assert!(
        text.contains("<key>ExitTimeOut</key>\n  <integer>30</integer>"),
        "{text}"
    );
}

fn env_of(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// The installer runs setup with `CSR_DREAMS_CONSENT=yes` and may pass `CSR_INSTALL_DIR`; both
/// describe that one run. Written into the agent, setup printed "Carried into the daemon agent:
/// CSR_DREAMS_CONSENT=yes" and the daemon carried the answer forever.
#[test]
fn setup_only_variables_are_not_written_into_the_agent() {
    let carried = carried_process_env(
        [
            ("CSR_AUTO_SETUP", "1"),
            ("CSR_SKIP_SETUP", "1"),
            ("CSR_NO_DAEMON", "0"),
            ("CSR_DREAMS_CONSENT", "yes"),
            ("CSR_MODEL_LIMITS_CONSENT", "yes"),
            ("CSR_INSTALL_DIR", "/Users/x/.local/bin"),
            ("CSR_BUILD_GIT_SHA", "abc"),
            ("HOME", "/Users/x"),
            ("CSR_NO_RECAP", "1"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned())),
    );
    assert_eq!(carried, env_of(&[("CSR_NO_RECAP", "1")]));
    let mut spec = agent_spec(Path::new("/Users/x"));
    spec.env = carried;
    let text = render_daemon_plist(&spec);
    assert!(text.contains("<key>CSR_NO_RECAP</key>"), "{text}");
    for key in ["CSR_DREAMS_CONSENT", "CSR_INSTALL_DIR", "CSR_AUTO_SETUP"] {
        assert!(!text.contains(key), "{key} in the plist:\n{text}");
    }
}

/// An agent an earlier setup wrote with the installer's variables loses them on the next run,
/// from the file and from the loaded job alike; the daemon's own switches stay.
#[test]
fn setup_only_variables_an_older_agent_carries_are_dropped_on_rerun() {
    let older = env_of(&[
        ("CSR_DREAMS_CONSENT", "yes"),
        ("CSR_INSTALL_DIR", "/Users/x/.local/bin"),
        ("CSR_NO_DREAMING", "1"),
    ]);
    for (loaded, on_disk) in [(&older, Some(&older)), (&older, None)] {
        let merged = merge_agent_env(loaded, on_disk, &Default::default());
        let keys: Vec<&str> = merged.keys().map(String::as_str).collect();
        assert_eq!(keys, ["CSR_NO_DREAMING"], "on disk: {}", on_disk.is_some());
    }
}
