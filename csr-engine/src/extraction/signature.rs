//! Conversation signature — metadata for filtering and search.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::errors::ErrorContext;
use super::patterns::EditPattern;
use super::{content_to_lower, get_message_data};

/// Conversation-level metadata for filtering and enriched search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSignature {
    pub completion_status: String,
    pub confidence: String,
    pub frameworks: Vec<String>,
    pub pattern_reusability: String,
    pub error_recovery: bool,
    pub total_edits: usize,
    pub iteration_count: usize,
    pub tools_used: Vec<String>,
    pub files_modified: Vec<String>,
    pub concepts: Vec<String>,
}

/// Build a conversation signature from messages, errors, and edit patterns.
pub fn build_signature(
    messages: &[Value],
    errors: &[ErrorContext],
    patterns: &[EditPattern],
) -> ConversationSignature {
    let msg_data_list: Vec<Value> = messages.iter().map(|m| get_message_data(m)).collect();

    // Detect completion status from last 10 messages
    let last_10_start = msg_data_list.len().saturating_sub(10);
    let last_10 = &msg_data_list[last_10_start..];

    let has_build_success = last_10.iter().any(|m| {
        let c = content_to_lower(&m);
        c.contains("compiled successfully") || (c.contains("build") && c.contains("success"))
    });
    let has_test_success = last_10
        .iter()
        .any(|m| {
            let c = content_to_lower(&m);
            c.contains("test") && c.contains("pass")
        });
    let has_completion = last_10.iter().any(|m| {
        let c = content_to_lower(&m);
        c.contains("all tasks completed")
            || (c.contains("successfully")
                && (c.contains("deployment") || c.contains("completed")))
    });

    // Only count truly blocking unresolved errors in last 20% of conversation
    let last_20_pct = (messages.len() as f64 * 0.8) as usize;
    let blocking_errors: Vec<&ErrorContext> = errors
        .iter()
        .filter(|e| {
            !e.resolved
                && !e.error_text.to_lowercase().contains("todowrite")
                && e.error_text.trim().len() > 20
                && e.index > last_20_pct
                && !e.error_text.to_lowercase().contains("vercel")
                && !e.error_text.contains("http://")
                && !e.error_text.contains("https://")
        })
        .collect();

    let completion_status =
        if (has_build_success || has_test_success || has_completion) && blocking_errors.is_empty() {
            "success"
        } else if !blocking_errors.is_empty() {
            "failed"
        } else {
            "partial"
        };

    // Detect frameworks
    let all_content: String = msg_data_list
        .iter()
        .map(|m| content_to_lower(&m))
        .collect::<Vec<_>>()
        .join(" ");

    let mut frameworks = Vec::new();
    if all_content.contains("react") || all_content.contains("jsx") {
        frameworks.push("react".to_string());
    }
    if all_content.contains("next.js") || all_content.contains("nextjs") {
        frameworks.push("nextjs".to_string());
    }
    if all_content.contains("typescript") || all_content.contains(".tsx") || all_content.contains(".ts ") {
        frameworks.push("typescript".to_string());
    }
    if all_content.contains("python") || all_content.contains(".py") {
        frameworks.push("python".to_string());
    }
    if all_content.contains("rust") || all_content.contains("cargo") {
        frameworks.push("rust".to_string());
    }

    // Pattern reusability
    let high_value = ["cascade_updates", "removal", "refactor"];
    let pattern_reusability = if patterns
        .iter()
        .any(|p| high_value.contains(&p.operation_type.as_str()))
    {
        "high"
    } else {
        "medium"
    };

    // Error recovery
    let error_recovery = errors.iter().any(|e| e.resolved);

    // Extract tools used from messages
    let mut tools_used = Vec::new();
    for m in &msg_data_list {
        if let Some(content) = m.get("content").and_then(|v| v.as_array()) {
            for item in content {
                if item.get("type").and_then(|v| v.as_str()) == Some("tool_use") {
                    if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                        if !tools_used.contains(&name.to_string()) {
                            tools_used.push(name.to_string());
                        }
                    }
                }
            }
        }
    }
    tools_used.truncate(10);

    // Extract files modified from patterns
    let files_modified: Vec<String> = patterns
        .iter()
        .map(|p| p.file.clone())
        .filter(|f| f != "unknown")
        .collect::<Vec<_>>()
        .into_iter()
        .take(10)
        .collect();

    // Count user messages as iterations
    let iteration_count = msg_data_list
        .iter()
        .filter(|m| m.get("role").and_then(|v| v.as_str()) == Some("user"))
        .count();

    ConversationSignature {
        completion_status: completion_status.to_string(),
        confidence: "MEDIUM".to_string(),
        frameworks,
        pattern_reusability: pattern_reusability.to_string(),
        error_recovery,
        total_edits: patterns.len(),
        iteration_count,
        tools_used,
        files_modified,
        concepts: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_success_detection() {
        let messages = vec![
            json!({"role": "user", "content": "Fix the bug in auth module that is causing login failures for users"}),
            json!({"role": "assistant", "content": "Build compiled successfully. All tests pass."}),
        ];
        let sig = build_signature(&messages, &[], &[]);
        assert_eq!(sig.completion_status, "success");
    }

    #[test]
    fn test_failure_detection() {
        let messages: Vec<Value> = (0..10)
            .map(|_| json!({"role": "assistant", "content": "still working on it"}))
            .collect();
        let errors = vec![ErrorContext {
            index: 9,
            error_text: "fatal: connection refused to database server on port 5432".to_string(),
            resolved: false,
            resolution: None,
        }];
        let sig = build_signature(&messages, &errors, &[]);
        assert_eq!(sig.completion_status, "failed");
    }

    #[test]
    fn test_framework_detection() {
        let messages = vec![
            json!({"role": "user", "content": "Fix the React component in the TypeScript project that handles user authentication"}),
        ];
        let sig = build_signature(&messages, &[], &[]);
        assert!(sig.frameworks.contains(&"react".to_string()));
        assert!(sig.frameworks.contains(&"typescript".to_string()));
    }

    #[test]
    fn test_tools_extraction() {
        let messages = vec![json!({
            "role": "assistant",
            "content": [
                {"type": "tool_use", "name": "Edit", "input": {}},
                {"type": "tool_use", "name": "Bash", "input": {}}
            ]
        })];
        let sig = build_signature(&messages, &[], &[]);
        assert!(sig.tools_used.contains(&"Edit".to_string()));
        assert!(sig.tools_used.contains(&"Bash".to_string()));
    }
}
