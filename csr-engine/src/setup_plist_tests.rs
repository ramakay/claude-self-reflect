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
