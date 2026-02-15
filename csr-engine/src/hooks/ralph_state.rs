//! Ralph session state parser — supports two file formats:
//! 1. Ralph Wiggum plugin (`.claude/ralph-loop.local.md`) — YAML frontmatter
//! 2. Custom state (`.ralph_state.md`) — markdown with `## Metadata` section

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;

// Pre-compiled regexes for error signature normalization (S-1 fix)
static RE_TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})?")
        .unwrap()
});
static RE_HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"0x[0-9a-fA-F]+").unwrap());
static RE_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?:at )?line \d+").unwrap());
static RE_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:/[\w._-]+){2,}(?:\.\w+)?").unwrap());
static RE_LINECOL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r":\d+(?::\d+)?").unwrap());
static RE_WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
static RE_ERROR_COUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(x(\d+)\)\s*$").unwrap());

/// Work type classification for a Ralph session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkType {
    Implementation,
    Testing,
    Debugging,
    Documentation,
    Unknown,
}

impl std::fmt::Display for WorkType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkType::Implementation => write!(f, "IMPLEMENTATION"),
            WorkType::Testing => write!(f, "TESTING"),
            WorkType::Debugging => write!(f, "DEBUGGING"),
            WorkType::Documentation => write!(f, "DOCUMENTATION"),
            WorkType::Unknown => write!(f, "UNKNOWN"),
        }
    }
}

/// Session outcome determination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Incomplete,
    Abandoned,
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::Completed => write!(f, "completed"),
            Outcome::Incomplete => write!(f, "incomplete"),
            Outcome::Abandoned => write!(f, "abandoned"),
        }
    }
}

/// Parsed Ralph session state.
#[derive(Debug, Clone)]
pub struct RalphState {
    pub session_id: String,
    pub task: String,
    pub iteration: usize,
    pub active: bool,
    pub work_type: WorkType,
    pub exit_confidence: u8,
    pub completion_promise: Option<String>,
    pub completion_promise_met: bool,
    pub failed_approaches: Vec<String>,
    pub successful_strategies: Vec<String>,
    pub error_signatures: Vec<(String, usize)>, // (normalized_error, count)
    pub files_modified: Vec<String>,
    pub learnings: Vec<String>,
}

impl Default for RalphState {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            task: String::new(),
            iteration: 0,
            active: false,
            work_type: WorkType::Unknown,
            exit_confidence: 0,
            completion_promise: None,
            completion_promise_met: false,
            failed_approaches: Vec::new(),
            successful_strategies: Vec::new(),
            error_signatures: Vec::new(),
            files_modified: Vec::new(),
            learnings: Vec::new(),
        }
    }
}

impl RalphState {
    /// Parse Ralph state from a file path.
    /// Returns None if the file doesn't exist.
    pub fn from_file(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(path)?;
        if content.trim().is_empty() {
            return Ok(None);
        }

        // Detect format: YAML frontmatter starts with "---"
        if content.starts_with("---") {
            Self::parse_plugin_format(&content)
        } else {
            Self::parse_custom_format(&content)
        }
    }

    /// Detect Ralph state by checking both known file paths.
    /// Checks plugin format first (`.claude/ralph-loop.local.md`),
    /// then custom format (`.ralph_state.md`).
    pub fn detect() -> Result<Option<Self>> {
        let cwd = std::env::current_dir()?;

        // Check plugin format first
        let plugin_path = cwd.join(".claude").join("ralph-loop.local.md");
        if let Some(state) = Self::from_file(&plugin_path)? {
            if state.active {
                return Ok(Some(state));
            }
        }

        // Check custom format
        let custom_path = cwd.join(".ralph_state.md");
        if let Some(state) = Self::from_file(&custom_path)? {
            if state.active {
                return Ok(Some(state));
            }
        }

        Ok(None)
    }

    /// Detect Ralph state from a specific CWD (for testing or explicit path).
    pub fn detect_in(cwd: &Path) -> Result<Option<Self>> {
        let plugin_path = cwd.join(".claude").join("ralph-loop.local.md");
        if let Some(state) = Self::from_file(&plugin_path)? {
            if state.active {
                return Ok(Some(state));
            }
        }

        let custom_path = cwd.join(".ralph_state.md");
        if let Some(state) = Self::from_file(&custom_path)? {
            if state.active {
                return Ok(Some(state));
            }
        }

        Ok(None)
    }

    /// Determine session outcome based on state and reason.
    pub fn determine_outcome(&self, reason: &str) -> Outcome {
        if self.completion_promise_met {
            return Outcome::Completed;
        }
        match reason {
            "clear" | "logout" => Outcome::Abandoned,
            _ => Outcome::Incomplete,
        }
    }

    /// Parse the Ralph Wiggum plugin format (YAML frontmatter + body).
    fn parse_plugin_format(content: &str) -> Result<Option<Self>> {
        // Extract YAML between --- delimiters
        let parts: Vec<&str> = content.splitn(3, "---").collect();
        if parts.len() < 3 {
            return Ok(None);
        }

        let yaml_str = parts[1].trim();
        let body = parts[2].trim();

        // Parse YAML frontmatter
        let yaml: serde_yml::Value = serde_yml::from_str(yaml_str)
            .unwrap_or_else(|_| serde_yml::Value::Mapping(serde_yml::Mapping::new()));

        let active = yaml
            .get("active")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let iteration = yaml
            .get("iteration")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        let max_iterations = yaml
            .get("max_iterations")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        let completion_promise = yaml
            .get("completion_promise")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let started_at = yaml
            .get("started_at")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Generate session ID from started_at (C-2 fix: use full timestamp for uniqueness)
        let session_id = if !started_at.is_empty() {
            format!(
                "ralph_{}",
                started_at.replace([':', '-', 'T', 'Z', '.', '+'], "")
            )
        } else {
            // Fallback: include UUID fragment for uniqueness
            format!(
                "ralph_plugin_{}_{}",
                iteration,
                &uuid::Uuid::new_v4().to_string()[..8]
            )
        };

        let _ = max_iterations; // Available but not stored in struct

        let mut state = RalphState {
            session_id,
            task: body.lines().next().unwrap_or("").to_string(),
            iteration,
            active,
            completion_promise,
            ..Default::default()
        };

        // Parse body for additional sections
        Self::parse_body_sections(body, &mut state);

        Ok(Some(state))
    }

    /// Parse the custom `.ralph_state.md` markdown format.
    /// Note: `active` defaults to `false`; the file must contain `Active: true`
    /// or the session won't be detected by `detect_in()`.
    fn parse_custom_format(content: &str) -> Result<Option<Self>> {
        let mut state = RalphState::default();
        // active defaults to false (C-1 fix); file must declare Active: true

        let mut current_section = String::new();

        for line in content.lines() {
            let trimmed = line.trim();

            if trimmed.starts_with("## ") {
                current_section = trimmed[3..].to_lowercase();
                continue;
            }

            match current_section.as_str() {
                "metadata" => {
                    if let Some(val) = extract_metadata_value(trimmed, "Session ID:") {
                        state.session_id = val;
                    } else if let Some(val) = extract_metadata_value(trimmed, "Task:") {
                        state.task = val;
                    } else if let Some(val) = extract_metadata_value(trimmed, "Iteration:") {
                        state.iteration = val.parse().unwrap_or(0);
                    } else if let Some(val) = extract_metadata_value(trimmed, "Work Type:") {
                        state.work_type = parse_work_type(&val);
                    } else if let Some(val) = extract_metadata_value(trimmed, "Exit Confidence:") {
                        state.exit_confidence =
                            val.trim_end_matches('%').parse().unwrap_or(0);
                    } else if let Some(val) = extract_metadata_value(trimmed, "Active:") {
                        state.active = val.to_lowercase() == "true" || val.to_lowercase() == "yes";
                    }
                }
                s if s.contains("failed approaches") || s.contains("do not retry") => {
                    if trimmed.starts_with("- ") {
                        state.failed_approaches.push(trimmed[2..].to_string());
                    }
                }
                s if s.contains("successful") || s.contains("winning") => {
                    if trimmed.starts_with("- ") {
                        state
                            .successful_strategies
                            .push(trimmed[2..].to_string());
                    }
                }
                s if s.contains("error signatures") || s.contains("errors") => {
                    if trimmed.starts_with("- ") {
                        let sig_text = &trimmed[2..];
                        let (sig, count) = parse_error_with_count(sig_text);
                        state.error_signatures.push((normalize_error_signature(&sig), count));
                    }
                }
                s if s.contains("files modified") || s.contains("files changed") => {
                    if trimmed.starts_with("- ") {
                        state.files_modified.push(trimmed[2..].to_string());
                    }
                }
                s if s.contains("learnings") || s.contains("insights") => {
                    if trimmed.starts_with("- ") {
                        state.learnings.push(trimmed[2..].to_string());
                    }
                }
                _ => {}
            }
        }

        // Generate session_id if not found
        if state.session_id.is_empty() {
            state.session_id = format!("ralph_custom_{}", state.iteration);
        }

        Ok(Some(state))
    }

    /// Parse body sections that may appear in either format.
    fn parse_body_sections(body: &str, state: &mut RalphState) {
        let mut current_section = String::new();

        for line in body.lines() {
            let trimmed = line.trim();

            if trimmed.starts_with("## ") || trimmed.starts_with("### ") {
                current_section = trimmed
                    .trim_start_matches('#')
                    .trim()
                    .to_lowercase();
                continue;
            }

            if !trimmed.starts_with("- ") {
                continue;
            }

            let item = trimmed[2..].to_string();

            if current_section.contains("failed") || current_section.contains("don't retry") || current_section.contains("do not retry") {
                state.failed_approaches.push(item);
            } else if current_section.contains("successful") || current_section.contains("winning") {
                state.successful_strategies.push(item);
            } else if current_section.contains("error") {
                let (sig, count) = parse_error_with_count(&item);
                state.error_signatures.push((normalize_error_signature(&sig), count));
            } else if current_section.contains("files") {
                state.files_modified.push(item);
            } else if current_section.contains("learning") || current_section.contains("insight") {
                state.learnings.push(item);
            }
        }
    }

    /// Serialize state to a text narrative for storage.
    pub fn to_narrative(&self, outcome: &Outcome) -> String {
        let mut out = String::new();

        out.push_str(&format!("RALPH SESSION: {}\n", self.session_id));
        out.push_str(&format!("TASK: {}\n", self.task));
        out.push_str(&format!("OUTCOME: {}\n", outcome));
        out.push_str(&format!("ITERATIONS: {}\n", self.iteration));
        out.push_str(&format!("WORK TYPE: {}\n", self.work_type));
        out.push_str(&format!("EXIT CONFIDENCE: {}%\n", self.exit_confidence));

        if let Some(promise) = &self.completion_promise {
            out.push_str(&format!("COMPLETION PROMISE: {}\n", promise));
            out.push_str(&format!(
                "PROMISE MET: {}\n",
                if self.completion_promise_met {
                    "yes"
                } else {
                    "no"
                }
            ));
        }

        if !self.failed_approaches.is_empty() {
            out.push_str("\nFAILED APPROACHES (DO NOT RETRY):\n");
            for approach in &self.failed_approaches {
                out.push_str(&format!("- {}\n", approach));
            }
        }

        if !self.successful_strategies.is_empty() {
            out.push_str("\nSUCCESSFUL STRATEGIES:\n");
            for strategy in &self.successful_strategies {
                out.push_str(&format!("- {}\n", strategy));
            }
        }

        if !self.error_signatures.is_empty() {
            out.push_str("\nERROR SIGNATURES:\n");
            for (sig, count) in &self.error_signatures {
                out.push_str(&format!("- {} (x{})\n", sig, count));
            }
        }

        if !self.files_modified.is_empty() {
            out.push_str("\nFILES MODIFIED:\n");
            for file in &self.files_modified {
                out.push_str(&format!("- {}\n", file));
            }
        }

        if !self.learnings.is_empty() {
            out.push_str("\nLEARNINGS:\n");
            for learning in &self.learnings {
                out.push_str(&format!("- {}\n", learning));
            }
        }

        out
    }
}

/// Normalize an error signature by removing variable parts:
/// - Line numbers (`at line 42`, `:42:`, `line 42`)
/// - File paths (`/Users/foo/bar.rs`)
/// - Timestamps (`2026-01-04T10:36:30Z`)
/// - Hex addresses (`0x7fff5fbff8a0`)
pub fn normalize_error_signature(error: &str) -> String {
    // Cap input length to prevent excessive regex work (S-1 fix)
    let input = if error.len() > 1024 {
        &error[..1024]
    } else {
        error
    };
    let mut s = input.to_string();

    // ORDER MATTERS: match specific patterns before general ones
    // Uses pre-compiled LazyLock regexes for zero-cost reuse

    // 1. Remove ISO timestamps FIRST (before paths/linecol consume the digits)
    s = RE_TIMESTAMP.replace_all(&s, "<TIMESTAMP>").to_string();
    // 2. Remove hex addresses (before paths consume hex-like segments)
    s = RE_HEX.replace_all(&s, "<ADDR>").to_string();
    // 3. Remove "at line N" / "line N" (before linecol)
    s = RE_LINE.replace_all(&s, "line <N>").to_string();
    // 4. Remove file paths (Unix-style: /foo/bar.rs)
    s = RE_PATH.replace_all(&s, "<PATH>").to_string();
    // 5. Remove line:col references like `:42:13` or `:42`
    s = RE_LINECOL.replace_all(&s, ":<N>").to_string();
    // 6. Collapse whitespace
    s = RE_WHITESPACE.replace_all(&s, " ").to_string();

    s.trim().to_string()
}

/// Extract a metadata value from a line like `- **Key:** value`.
fn extract_metadata_value(line: &str, key: &str) -> Option<String> {
    // Match patterns like "- **Key:** value" or "- Key: value"
    let stripped = line
        .trim_start_matches("- ")
        .replace("**", "");

    if let Some(rest) = stripped.strip_prefix(key) {
        Some(rest.trim().to_string())
    } else {
        None
    }
}

/// Parse a work type string.
fn parse_work_type(s: &str) -> WorkType {
    match s.to_uppercase().trim() {
        "IMPLEMENTATION" => WorkType::Implementation,
        "TESTING" => WorkType::Testing,
        "DEBUGGING" => WorkType::Debugging,
        "DOCUMENTATION" => WorkType::Documentation,
        _ => WorkType::Unknown,
    }
}

/// Parse an error signature with optional count, e.g. "`JWT expired` (x3)".
fn parse_error_with_count(text: &str) -> (String, usize) {
    if let Some(caps) = RE_ERROR_COUNT.captures(text) {
        let count: usize = caps[1].parse().unwrap_or(1);
        let sig = RE_ERROR_COUNT.replace(text, "").trim().to_string();
        // Strip backticks
        let sig = sig.trim_matches('`').to_string();
        (sig, count)
    } else {
        let sig = text.trim_matches('`').to_string();
        (sig, 1)
    }
}

/// Known Ralph state file paths relative to a working directory.
pub fn state_file_paths(cwd: &Path) -> Vec<PathBuf> {
    vec![
        cwd.join(".claude").join("ralph-loop.local.md"),
        cwd.join(".ralph_state.md"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_parse_plugin_format() {
        let content = r#"---
active: true
iteration: 5
max_iterations: 50
completion_promise: "Tests passing"
started_at: "2026-01-04T04:25:46Z"
---
Fix the authentication bug in login flow

## Failed Approaches (DO NOT RETRY)
- Token refresh hack
- Direct cookie manipulation

## Error Signatures
- `JWT expired at line 42` (x3)
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("state.md");
        std::fs::write(&path, content).unwrap();

        let state = RalphState::from_file(&path).unwrap().unwrap();
        assert!(state.active);
        assert_eq!(state.iteration, 5);
        assert_eq!(state.task, "Fix the authentication bug in login flow");
        assert!(state.completion_promise.as_deref() == Some("Tests passing"));
        assert_eq!(state.failed_approaches.len(), 2);
        assert_eq!(state.failed_approaches[0], "Token refresh hack");
        assert_eq!(state.error_signatures.len(), 1);
        assert_eq!(state.error_signatures[0].1, 3); // count
    }

    #[test]
    fn test_parse_custom_format() {
        let content = r#"## Metadata
- **Session ID:** ralph_20260104_224757
- **Task:** Fix auth bug
- **Iteration:** 8
- **Work Type:** DEBUGGING
- **Exit Confidence:** 75%
- **Active:** true

## Failed Approaches (DO NOT RETRY)
- Token refresh hack

## Error Signatures (Deduplicated)
- `JWT expired at line N` (x3)

## Files Modified
- src/auth.rs
- tests/auth_test.rs

## Learnings
- Always check token expiry before refresh
"#;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".ralph_state.md");
        std::fs::write(&path, content).unwrap();

        let state = RalphState::from_file(&path).unwrap().unwrap();
        assert!(state.active);
        assert_eq!(state.session_id, "ralph_20260104_224757");
        assert_eq!(state.task, "Fix auth bug");
        assert_eq!(state.iteration, 8);
        assert_eq!(state.work_type, WorkType::Debugging);
        assert_eq!(state.exit_confidence, 75);
        assert_eq!(state.failed_approaches.len(), 1);
        assert_eq!(state.error_signatures.len(), 1);
        assert_eq!(state.files_modified.len(), 2);
        assert_eq!(state.learnings.len(), 1);
    }

    #[test]
    fn test_normalize_error_signature() {
        // Line numbers removed
        assert_eq!(
            normalize_error_signature("error at line 42 in module"),
            "error line <N> in module"
        );

        // File paths removed
        let normalized = normalize_error_signature("failed to read /Users/foo/bar.rs");
        assert!(normalized.contains("<PATH>"));
        assert!(!normalized.contains("/Users"));

        // Timestamps removed
        let normalized =
            normalize_error_signature("error at 2026-01-04T10:36:30Z in handler");
        assert!(normalized.contains("<TIMESTAMP>"));

        // Hex addresses removed
        let normalized = normalize_error_signature("segfault at 0x7fff5fbff8a0");
        assert!(normalized.contains("<ADDR>"));
    }

    #[test]
    fn test_determine_outcome() {
        let mut state = RalphState::default();

        // Default incomplete
        assert_eq!(state.determine_outcome("shutdown"), Outcome::Incomplete);

        // Abandoned on clear/logout
        assert_eq!(state.determine_outcome("clear"), Outcome::Abandoned);
        assert_eq!(state.determine_outcome("logout"), Outcome::Abandoned);

        // Completed when promise met
        state.completion_promise_met = true;
        assert_eq!(state.determine_outcome("shutdown"), Outcome::Completed);
    }

    #[test]
    fn test_detect_in_directory() {
        let tmp = TempDir::new().unwrap();

        // No files → None
        let result = RalphState::detect_in(tmp.path()).unwrap();
        assert!(result.is_none());

        // Create custom state file with Active: true
        let content =
            "## Metadata\n- **Task:** Test\n- **Iteration:** 1\n- **Active:** true\n";
        let state_path = tmp.path().join(".ralph_state.md");
        let mut f = std::fs::File::create(&state_path).unwrap();
        f.write_all(content.as_bytes()).unwrap();

        let result = RalphState::detect_in(tmp.path()).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().task, "Test");
    }

    #[test]
    fn test_custom_format_defaults_inactive() {
        // C-1 fix: custom format without Active field should default to inactive
        let content = "## Metadata\n- **Task:** Test\n- **Iteration:** 1\n";
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(".ralph_state.md");
        std::fs::write(&path, content).unwrap();

        let state = RalphState::from_file(&path).unwrap().unwrap();
        assert!(!state.active, "custom format should default to active=false");

        // detect_in should NOT find this (inactive)
        let result = RalphState::detect_in(tmp.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_to_narrative() {
        let state = RalphState {
            session_id: "ralph_test_123".into(),
            task: "Fix the bug".into(),
            iteration: 5,
            active: true,
            work_type: WorkType::Debugging,
            exit_confidence: 80,
            completion_promise: Some("All tests pass".into()),
            completion_promise_met: true,
            failed_approaches: vec!["Hack approach".into()],
            successful_strategies: vec!["Proper fix".into()],
            error_signatures: vec![("JWT expired".into(), 3)],
            files_modified: vec!["src/auth.rs".into()],
            learnings: vec!["Check tokens first".into()],
        };

        let narrative = state.to_narrative(&Outcome::Completed);
        assert!(narrative.contains("RALPH SESSION: ralph_test_123"));
        assert!(narrative.contains("OUTCOME: completed"));
        assert!(narrative.contains("FAILED APPROACHES"));
        assert!(narrative.contains("Hack approach"));
        assert!(narrative.contains("SUCCESSFUL STRATEGIES"));
    }

    #[test]
    fn test_parse_error_with_count() {
        let (sig, count) = parse_error_with_count("`JWT expired` (x3)");
        assert_eq!(sig, "JWT expired");
        assert_eq!(count, 3);

        let (sig, count) = parse_error_with_count("simple error");
        assert_eq!(sig, "simple error");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_inactive_state_not_detected() {
        let tmp = TempDir::new().unwrap();

        // Create a state file with active: false
        let content = "## Metadata\n- **Task:** Test\n- **Active:** false\n";
        std::fs::write(tmp.path().join(".ralph_state.md"), content).unwrap();

        let result = RalphState::detect_in(tmp.path()).unwrap();
        assert!(result.is_none());
    }
}
