//! The daemon's launchd agent plist, as `csr-engine setup` writes it.

use std::path::Path;
use std::time::Duration;

use super::{DaemonAgentSpec, DAEMON_LABEL, DAEMON_PLIST_MARKER};

/// The plist's `ExitTimeOut`: how long launchd waits after SIGTERM before it sends SIGKILL.
/// Without the key launchd used 5 s (`launchctl print` read `exit timeout = 5` on 2026-11-17),
/// shorter than the daemon's graceful stop, so a stop could end in SIGKILL mid-flush. The
/// daemon bounds its loop joins under this (`daemon::stop_joins::JOIN_BUDGET`).
pub const DAEMON_EXIT_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The agent plist. Every path is absolute. The database and projects folder are
/// always passed explicitly, because `vector_ipc::launchd_database` reads the
/// database the daemon serves from `--db-path`.
pub(super) fn render_daemon_plist(spec: &DaemonAgentSpec) -> String {
    let s = |p: &Path| xml_escape(&p.to_string_lossy());
    let data_dir = spec.data_dir();
    let mut env = spec.env.clone();
    env.insert("PATH".to_owned(), spec.path_env());
    // Sorted (BTreeMap), so an unchanged install renders byte-identical text.
    let env: String = env
        .iter()
        .map(|(key, value)| {
            format!(
                "    <key>{}</key>\n    <string>{}</string>\n",
                xml_escape(key),
                xml_escape(value)
            )
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
{DAEMON_PLIST_MARKER}
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{DAEMON_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{engine}</string>
    <string>--db-path</string>
    <string>{db}</string>
    <string>--projects-dir</string>
    <string>{projects}</string>
    <string>daemon</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
{env}  </dict>
  <key>WorkingDirectory</key>
  <string>{data}</string>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ThrottleInterval</key>
  <integer>30</integer>
  <key>ExitTimeOut</key>
  <integer>{exit_timeout}</integer>
  <key>ProcessType</key>
  <string>Interactive</string>
  <key>Nice</key>
  <integer>10</integer>
  <key>StandardOutPath</key>
  <string>{out}</string>
  <key>StandardErrorPath</key>
  <string>{err}</string>
</dict>
</plist>
"#,
        engine = s(&spec.engine),
        db = s(&spec.db_path),
        projects = s(&spec.projects_dir),
        data = s(&data_dir),
        exit_timeout = DAEMON_EXIT_TIMEOUT.as_secs(),
        out = s(&data_dir.join("logs/daemon.out.log")),
        err = s(&data_dir.join("logs/daemon.err.log")),
    )
}
