//! Claude Code's own record of a running session: `~/.claude/sessions/<pid>.json`, written by
//! each Claude Code process under its pid, holding `sessionId` and `cwd`. A csr-engine process a
//! session started (an MCP server, a relay) has that session's pid as its parent, so the file
//! names the session and project of a process that carries neither on its command line.
//!
//! Best effort: a missing, unreadable or unparsable file is "unknown", never an error.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The session and project folder Claude Code recorded for one of its processes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeSession {
    pub session_id: String,
    pub cwd: Option<String>,
}

/// `$CLAUDE_CONFIG_DIR/sessions`, else `~/.claude/sessions`.
pub fn sessions_dir() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir).join("sessions")),
        _ => dirs::home_dir().map(|home| home.join(".claude").join("sessions")),
    }
}

/// The record `<dir>/<pid>.json`, when it holds a non-empty `sessionId`.
pub fn session_of(dir: &Path, pid: u32) -> Option<ClaudeSession> {
    let bytes = std::fs::read(dir.join(format!("{pid}.json"))).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let session_id = value.get("sessionId")?.as_str()?.trim();
    if session_id.is_empty() {
        return None;
    }
    Some(ClaudeSession {
        session_id: session_id.to_owned(),
        cwd: value
            .get("cwd")
            .and_then(|cwd| cwd.as_str())
            .filter(|cwd| !cwd.is_empty())
            .map(str::to_owned),
    })
}

/// The records of every pid in `pids` that has one.
pub fn sessions_of(dir: Option<&Path>, pids: &[u32]) -> HashMap<u32, ClaudeSession> {
    let Some(dir) = dir else {
        return HashMap::new();
    };
    pids.iter()
        .filter_map(|pid| Some((*pid, session_of(dir, *pid)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pid_file_names_the_session_and_folder_and_anything_else_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("3348.json"),
            r#"{"pid":3348,"sessionId":"e22fe55d-a6d6-40a9-b72c-040597c48324","cwd":"/p/anukriti-website","kind":"interactive"}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("5507.json"), "not json").unwrap();
        std::fs::write(dir.path().join("5508.json"), r#"{"cwd":"/p/x"}"#).unwrap();
        let found = sessions_of(Some(dir.path()), &[3348, 5507, 5508, 9999]);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[&3348],
            ClaudeSession {
                session_id: "e22fe55d-a6d6-40a9-b72c-040597c48324".into(),
                cwd: Some("/p/anukriti-website".into()),
            }
        );
        assert!(sessions_of(None, &[3348]).is_empty());
    }
}
