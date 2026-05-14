//! Search index and context cache builders.
//!
//! - `build_search_index`: ~500 tokens for vector embedding
//! - `build_context_cache`: ~1000 tokens stored as payload

use serde_json::Value;

use super::ast_analysis::CodeContext;
use super::errors::ErrorContext;
use super::get_message_data;
use super::patterns::EditPattern;

/// Build a ~500-token search index optimized for keyword matching.
///
/// Structure:
/// - User request (exact words)
/// - Solution type + tools used
/// - Files modified + operation types
/// - Code context (functions, types, imports, patterns)
pub fn build_search_index(
    messages: &[Value],
    patterns: &[EditPattern],
    errors: &[ErrorContext],
    code_context: &CodeContext,
) -> String {
    let mut parts = Vec::new();

    // Extract user requests (exclude tool_result noise)
    // Lowered threshold from 50→15 chars and increased limit from 2→4 to capture
    // short precise prompts and later pivots that would otherwise disappear.
    let mut user_requests = Vec::new();
    for msg in messages {
        let msg_data = get_message_data(msg);
        if msg_data.get("role").and_then(|v| v.as_str()) != Some("user") {
            continue;
        }
        let text = extract_user_text(&msg_data);
        if text.len() < 15 {
            continue;
        }
        if text.contains("tool_result")
            || text.contains("tool_use_id")
            || text.contains("<command-name>")
            || text.contains("Caveat:")
            || text.contains("<local-command")
        {
            continue;
        }
        let truncated: String = text.chars().take(200).collect();
        user_requests.push(truncated);
        if user_requests.len() >= 4 {
            break;
        }
    }

    if !user_requests.is_empty() {
        parts.push("## User Request".to_string());
        for req in &user_requests {
            parts.push(req.clone());
        }
        parts.push(String::new());
    }

    // Edit patterns
    if !patterns.is_empty() {
        parts.push("## Solution Pattern".to_string());
        for p in patterns.iter().take(3) {
            let file_short = p.file.rsplit('/').next().unwrap_or(&p.file);
            parts.push(format!("{}: {}", p.operation_type, file_short));
            parts.push(format!("  {}", p.pattern_description));
        }
        parts.push(String::new());
    }

    // Unresolved errors
    let unresolved: Vec<&ErrorContext> = errors.iter().filter(|e| !e.resolved).collect();
    if !unresolved.is_empty() {
        parts.push("## Active Issues".to_string());
        for err in unresolved.iter().take(2) {
            let truncated: String = err.error_text.chars().take(100).collect();
            parts.push(truncated);
        }
        parts.push(String::new());
    }

    // Code context from AST analysis
    if !code_context.is_empty() {
        let code_text = code_context.to_search_text();
        parts.push("## Code Context".to_string());
        parts.push(code_text);
        parts.push(String::new());
    }

    parts.join("\n")
}

/// Build a ~1000-token context cache with detailed implementation.
///
/// Structure:
/// - Full edit patterns with context
/// - Error→recovery sequences
/// - Build/test validation moments
pub fn build_context_cache(
    messages: &[Value],
    patterns: &[EditPattern],
    errors: &[ErrorContext],
) -> String {
    let mut parts = Vec::new();

    // Detailed edit patterns
    if !patterns.is_empty() {
        parts.push("## Implementation Details".to_string());
        for p in patterns.iter().take(5) {
            parts.push(format!("[Msg {}] {}", p.index, p.operation_type));
            parts.push(format!("  File: {}", p.file));
            parts.push(format!("  Pattern: {}", p.pattern_description));
            if p.why != "Unknown" {
                parts.push(format!("  Context: {}", p.why));
            }
        }
        parts.push(String::new());
    }

    // Error recovery sequences
    let resolved: Vec<&ErrorContext> = errors.iter().filter(|e| e.resolved).collect();
    if !resolved.is_empty() {
        parts.push("## Error Recovery".to_string());
        for err in resolved.iter().take(3) {
            let err_truncated: String = err.error_text.chars().take(100).collect();
            parts.push(format!("[Msg {}] Error: {}", err.index, err_truncated));
            if let Some(res) = &err.resolution {
                let res_truncated: String = res.chars().take(100).collect();
                parts.push(format!("  Fix: {}", res_truncated));
            }
        }
        parts.push(String::new());
    }

    // Key validation moments
    parts.push("## Validation".to_string());
    for (i, msg) in messages.iter().enumerate() {
        let msg_data = get_message_data(msg);
        let content_str = super::content_to_lower(&msg_data);

        if content_str.contains("compiled successfully")
            || (content_str.contains("build") && content_str.contains("success"))
        {
            parts.push(format!("[Msg {i}] Build: Success"));
        } else if content_str.contains("test") && content_str.contains("pass") {
            parts.push(format!("[Msg {i}] Tests: Passed"));
        }
    }

    parts.join("\n")
}

/// Extract user-visible text from a message, handling both string and array content formats.
fn extract_user_text(msg_data: &Value) -> String {
    match msg_data.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_search_index_includes_user_request() {
        let messages = vec![
            json!({"role": "user", "content": "Fix the authentication bug in the login flow that causes session timeout issues"}),
        ];
        let index = build_search_index(&messages, &[], &[], &CodeContext::default());
        assert!(index.contains("User Request"));
        assert!(index.contains("authentication"));
    }

    #[test]
    fn test_search_index_includes_patterns() {
        let patterns = vec![EditPattern {
            index: 1,
            file: "src/auth/login.rs".to_string(),
            operation_type: "modification".to_string(),
            pattern_description: "In-place modification".to_string(),
            why: "Fix auth".to_string(),
        }];
        let index = build_search_index(&[], &patterns, &[], &CodeContext::default());
        assert!(index.contains("Solution Pattern"));
        assert!(index.contains("login.rs"));
    }

    #[test]
    fn test_search_index_includes_code_context() {
        let mut code_ctx = CodeContext::default();
        code_ctx.functions.insert("dispatch_hook".to_string());
        code_ctx.types.insert("Engine".to_string());
        code_ctx.languages.insert("Rust".to_string());
        code_ctx.patterns.insert("async".to_string());

        let index = build_search_index(&[], &[], &[], &code_ctx);
        assert!(
            index.contains("Code Context"),
            "index should have Code Context section: {}",
            index
        );
        assert!(
            index.contains("dispatch_hook"),
            "index should contain function name: {}",
            index
        );
        assert!(
            index.contains("Engine"),
            "index should contain type name: {}",
            index
        );
    }

    #[test]
    fn test_search_index_includes_short_user_requests() {
        let messages = vec![
            json!({"role": "user", "content": "fix the auth bug"}),
            json!({"role": "assistant", "content": "Looking at auth..."}),
            json!({"role": "user", "content": "also check the session timeout in Redis"}),
        ];
        let index = build_search_index(&messages, &[], &[], &CodeContext::default());
        assert!(
            index.contains("auth"),
            "should include short request: {}",
            index
        );
        assert!(
            index.contains("session timeout") || index.contains("Redis"),
            "should include second user request: {}",
            index
        );
    }

    #[test]
    fn test_search_index_captures_up_to_four_requests() {
        let messages = vec![
            json!({"role": "user", "content": "first task: fix the login"}),
            json!({"role": "user", "content": "second task: update the API"}),
            json!({"role": "user", "content": "third task: add the tests"}),
            json!({"role": "user", "content": "fourth task: deploy it"}),
            json!({"role": "user", "content": "fifth task: should be excluded"}),
        ];
        let index = build_search_index(&messages, &[], &[], &CodeContext::default());
        assert!(index.contains("first task"));
        assert!(index.contains("fourth task"));
        assert!(!index.contains("fifth task"));
    }

    #[test]
    fn test_extract_user_text_string() {
        let msg = json!({"role": "user", "content": "hello world"});
        assert_eq!(super::extract_user_text(&msg), "hello world");
    }

    #[test]
    fn test_extract_user_text_array() {
        let msg = json!({"role": "user", "content": [
            {"type": "text", "text": "fix the bug"},
            {"type": "text", "text": "in auth.rs"}
        ]});
        assert_eq!(super::extract_user_text(&msg), "fix the bug in auth.rs");
    }

    #[test]
    fn test_context_cache_includes_recovery() {
        let errors = vec![ErrorContext {
            index: 2,
            error_text: "connection refused to database".to_string(),
            resolved: true,
            resolution: Some("Increased timeout to 120s".to_string()),
        }];
        let cache = build_context_cache(&[], &[], &errors);
        assert!(cache.contains("Error Recovery"));
        assert!(cache.contains("Increased timeout"));
    }

    #[test]
    fn test_context_cache_validation_moments() {
        let messages = vec![
            json!({"role": "assistant", "content": "Build compiled successfully."}),
            json!({"role": "assistant", "content": "All 57 tests pass."}),
        ];
        let cache = build_context_cache(&messages, &[], &[]);
        assert!(cache.contains("Build: Success"));
        assert!(cache.contains("Tests: Passed"));
    }
}
