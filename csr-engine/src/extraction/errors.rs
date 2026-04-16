//! Error context extraction with resolution tracking.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{content_to_lower, get_message_data};

/// An error with resolution tracking (15-message look-ahead).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorContext {
    pub index: usize,
    pub error_text: String,
    pub resolved: bool,
    pub resolution: Option<String>,
}

/// Extract error context from the message at `error_index`.
/// Checks the next 15 messages for resolution indicators.
pub fn extract_error_context(messages: &[Value], error_index: usize) -> ErrorContext {
    let msg_data = get_message_data(&messages[error_index]);
    let content = msg_data.get("content");

    // Extract clean error text
    let error_text = match content {
        Some(Value::Array(items)) => {
            let mut parts = Vec::new();
            for item in items {
                if let Some(obj) = item.as_object() {
                    if obj.get("type").and_then(|v| v.as_str()) == Some("tool_result") {
                        let result_content = obj
                            .get("content")
                            .map(|v| serde_json::to_string(v).unwrap_or_default())
                            .unwrap_or_default();
                        if result_content.to_lowercase().contains("error") {
                            let truncated: String = result_content.chars().take(300).collect();
                            parts.push(truncated);
                        }
                    }
                } else if let Some(s) = item.as_str() {
                    let truncated: String = s.chars().take(300).collect();
                    parts.push(truncated);
                }
            }
            parts.join(" ")
        }
        Some(v) => {
            let s = serde_json::to_string(v).unwrap_or_default();
            s.chars().take(300).collect()
        }
        None => String::new(),
    };

    // Check resolution in next 15 messages
    let mut resolved = false;
    let mut resolution = None;
    let end = messages.len().min(error_index + 15);
    let error_lower = error_text.to_lowercase();

    for msg in &messages[(error_index + 1)..end] {
        let check_data = get_message_data(msg);
        let check_str = content_to_lower(&check_data);

        // Explicit resolution
        if ["fixed", "solved", "working"]
            .iter()
            .any(|w| check_str.contains(w))
        {
            resolved = true;
            resolution = Some(check_str.chars().take(200).collect());
            break;
        }

        // Implicit: server started after connection_refused
        if error_lower.contains("connection_refused")
            && ((check_str.contains("background") && check_str.contains("running"))
                || (check_str.contains("playwright")
                    && !check_str.contains("success")
                    && !check_str.contains("error")))
        {
            resolved = true;
            resolution = Some("Server started / page loaded successfully".to_string());
            break;
        }

        // Build success after build error
        if (error_lower.contains("build") || error_lower.contains("compil"))
            && check_str.contains("compiled successfully")
        {
            resolved = true;
            resolution = Some("Build succeeded".to_string());
            break;
        }
    }

    ErrorContext {
        index: error_index,
        error_text,
        resolved,
        resolution,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_error_resolution_detected() {
        let messages = vec![
            json!({"role": "assistant", "content": "error: cannot find module 'auth'"}),
            json!({"role": "assistant", "content": "I fixed the import path. It's working now."}),
        ];
        let ctx = extract_error_context(&messages, 0);
        assert!(ctx.resolved);
        assert!(ctx.resolution.is_some());
    }

    #[test]
    fn test_error_unresolved() {
        let messages = vec![
            json!({"role": "assistant", "content": "error: cannot find module 'auth'"}),
            json!({"role": "assistant", "content": "Let me investigate further."}),
        ];
        let ctx = extract_error_context(&messages, 0);
        assert!(!ctx.resolved);
    }

    #[test]
    fn test_build_error_resolved_by_success() {
        let messages = vec![
            json!({"role": "assistant", "content": "build error: missing semicolon in compilation"}),
            json!({"role": "assistant", "content": "Added the semicolon."}),
            json!({"role": "assistant", "content": "Build compiled successfully."}),
        ];
        let ctx = extract_error_context(&messages, 0);
        assert!(ctx.resolved);
        assert_eq!(ctx.resolution.as_deref(), Some("Build succeeded"));
    }
}
