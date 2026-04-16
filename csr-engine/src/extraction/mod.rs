//! V3 Smart Event Extraction — port of extract_events_v3.py
//!
//! Produces a 500-token search index + 1000-token context cache per conversation.
//! Pure computation, no IO.

pub mod ast_analysis;
pub mod errors;
pub mod heuristic;
pub mod index_builder;
pub mod patterns;
pub mod quality;
pub mod scoring;
pub mod signature;
pub mod story;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use errors::ErrorContext;
pub use patterns::EditPattern;
pub use signature::ConversationSignature;

/// Full extraction result for a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionResult {
    pub search_index: String,
    pub context_cache: String,
    pub signature: ConversationSignature,
    pub code_context: ast_analysis::CodeContext,
    pub stats: ExtractionStats,
}

/// Statistics about the extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionStats {
    pub original_messages: usize,
    pub search_index_tokens: usize,
    pub context_cache_tokens: usize,
    pub total_tokens: usize,
    pub patterns_found: usize,
    pub errors_found: usize,
}

/// Helper: extract the message data handling nested structure.
/// JSONL messages can be `{"message": {...}}` or just `{...}`.
/// Also maps Claude's `type` field to `role` for compatibility.
pub fn get_message_data(msg: &Value) -> Value {
    let base = msg.get("message").unwrap_or(msg).clone();

    // If the message data has no "role" but the outer has "type", inject role
    if base.get("role").is_none() {
        if let Some(msg_type) = msg.get("type").and_then(|v| v.as_str()) {
            let role = match msg_type {
                "human" => "user",
                "assistant" => "assistant",
                _ => return base,
            };
            if let Value::Object(mut map) = base {
                map.insert("role".to_string(), Value::String(role.to_string()));
                return Value::Object(map);
            }
        }
    }

    base
}

/// Helper: get content as a lowercase string for keyword matching.
pub fn content_to_lower(msg_data: &Value) -> String {
    match msg_data.get("content") {
        Some(v) => serde_json::to_string(v).unwrap_or_default().to_lowercase(),
        None => String::new(),
    }
}

/// Run V3 extraction on a list of JSONL messages.
pub fn extract_v3(messages: &[Value]) -> ExtractionResult {
    // Score all messages
    let total = messages.len();
    let mut scored: Vec<(usize, f64)> = messages
        .iter()
        .enumerate()
        .map(|(i, msg)| (i, scoring::calculate_importance(msg, i, total)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let top_indices: Vec<usize> = scored.iter().take(20).map(|(i, _)| *i).collect();

    // Extract edit patterns from top-scored assistant messages
    let mut edit_patterns = Vec::new();
    for &i in &top_indices {
        let msg_data = get_message_data(&messages[i]);
        if msg_data.get("role").and_then(|v| v.as_str()) != Some("assistant") {
            continue;
        }
        if let Some(content) = msg_data.get("content").and_then(|v| v.as_array()) {
            for item in content {
                if item.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
                    continue;
                }
                let tool_name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                if (tool_name.contains("edit") || tool_name.contains("write"))
                    && !tool_name.contains("todo")
                {
                    edit_patterns.push(patterns::extract_edit_pattern(messages, i));
                }
            }
        }
    }

    // Extract errors
    let mut error_contexts = Vec::new();
    let error_keywords = ["error", "exception", "failed"];
    for (i, msg) in messages.iter().enumerate() {
        let content_str = content_to_lower(&get_message_data(msg));
        if error_keywords.iter().any(|kw| content_str.contains(kw)) {
            error_contexts.push(errors::extract_error_context(messages, i));
        }
    }

    // AST-based code context extraction
    let code_context = ast_analysis::extract_code_context(messages);

    // Build outputs (search index now includes code context)
    let search_index =
        index_builder::build_search_index(messages, &edit_patterns, &error_contexts, &code_context);
    let context_cache =
        index_builder::build_context_cache(messages, &edit_patterns, &error_contexts);
    let mut conv_signature = signature::build_signature(messages, &error_contexts, &edit_patterns);

    // Enrich signature concepts from AST analysis
    if !code_context.is_empty() {
        conv_signature
            .concepts
            .extend(code_context.patterns.iter().cloned());
        conv_signature
            .concepts
            .extend(code_context.languages.iter().map(|l| format!("lang:{l}")));
    }

    let search_tokens = search_index.len() / 4;
    let cache_tokens = context_cache.len() / 4;

    ExtractionResult {
        search_index,
        context_cache,
        signature: conv_signature,
        code_context,
        stats: ExtractionStats {
            original_messages: total,
            search_index_tokens: search_tokens,
            context_cache_tokens: cache_tokens,
            total_tokens: search_tokens + cache_tokens,
            patterns_found: edit_patterns.len(),
            errors_found: error_contexts.len(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_user_msg(content: &str) -> Value {
        json!({"role": "user", "content": content})
    }

    fn make_assistant_msg_with_edit(file: &str) -> Value {
        json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "name": "Edit",
                "input": {
                    "file_path": file,
                    "old_string": "old code here that is being replaced",
                    "new_string": "new code here that replaces old code with much more content added"
                }
            }]
        })
    }

    fn make_assistant_text(content: &str) -> Value {
        json!({"role": "assistant", "content": content})
    }

    #[test]
    fn test_extract_v3_basic() {
        let messages = vec![
            make_user_msg("Please fix the authentication bug in the login flow that causes users to be logged out unexpectedly"),
            make_assistant_msg_with_edit("src/auth.rs"),
            make_assistant_text("I've fixed the authentication bug. The issue was in the session validation logic. Build compiled successfully."),
        ];
        let result = extract_v3(&messages);
        assert_eq!(result.stats.original_messages, 3);
        assert!(result.stats.patterns_found >= 1);
        assert!(!result.search_index.is_empty());
        assert!(!result.context_cache.is_empty());
    }

    #[test]
    fn test_extract_v3_with_errors() {
        let messages = vec![
            make_user_msg("Fix the database connection error that keeps happening in the production environment"),
            make_assistant_text("I see the error: connection refused on port 5432. Let me investigate."),
            make_assistant_msg_with_edit("src/db.rs"),
            make_assistant_text("Fixed the connection pool. Build compiled successfully and all tests pass now."),
        ];
        let result = extract_v3(&messages);
        assert!(result.stats.errors_found >= 1);
    }

    #[test]
    fn test_get_message_data_nested() {
        let nested = json!({"message": {"role": "user", "content": "hello"}});
        let flat = json!({"role": "user", "content": "hello"});
        assert_eq!(
            get_message_data(&nested).get("role").unwrap().as_str(),
            Some("user")
        );
        assert_eq!(
            get_message_data(&flat).get("role").unwrap().as_str(),
            Some("user")
        );
    }
}
