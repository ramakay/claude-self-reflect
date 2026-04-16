//! Edit pattern extraction — classifies edits as reusable patterns.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::get_message_data;

/// A reusable edit pattern extracted from a tool_use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditPattern {
    pub index: usize,
    pub file: String,
    pub operation_type: String,
    pub pattern_description: String,
    pub why: String,
}

/// Extract an edit as a reusable pattern from the message at `edit_index`.
pub fn extract_edit_pattern(messages: &[Value], edit_index: usize) -> EditPattern {
    let msg_data = get_message_data(&messages[edit_index]);

    let mut pattern = EditPattern {
        index: edit_index,
        file: "unknown".to_string(),
        operation_type: "unknown".to_string(),
        pattern_description: String::new(),
        why: "Unknown".to_string(),
    };

    let content = match msg_data.get("content").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => return pattern,
    };

    for item in content {
        if item.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
            continue;
        }
        let tool_name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let tool_lower = tool_name.to_lowercase();
        if !tool_lower.contains("edit") && !tool_lower.contains("write") {
            continue;
        }
        if tool_lower.contains("todo") {
            continue;
        }

        let tool_input = item
            .get("input")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));

        // Extract file path
        if let Some(fp) = tool_input.get("file_path").and_then(|v| v.as_str()) {
            pattern.file = fp.to_string();
        }

        // Determine operation type
        match tool_name {
            "MultiEdit" => {
                let edits = tool_input
                    .get("edits")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let edits_str = tool_input
                    .get("edits")
                    .map(|v| serde_json::to_string(v).unwrap_or_default().to_lowercase())
                    .unwrap_or_default();

                if edits > 5 {
                    pattern.operation_type = "cascade_updates".to_string();
                    pattern.pattern_description =
                        format!("Batch operation: {edits} coordinated changes");
                } else if edits_str.contains("remove") || edits_str.contains("delete") {
                    pattern.operation_type = "removal".to_string();
                    pattern.pattern_description = "Item removal with cascade cleanup".to_string();
                } else {
                    pattern.operation_type = "refactor".to_string();
                    pattern.pattern_description =
                        format!("Multi-point refactoring ({edits} changes)");
                }
            }
            "Edit" => {
                let old_len = tool_input
                    .get("old_string")
                    .and_then(|v| v.as_str())
                    .map(|s| s.len())
                    .unwrap_or(0);
                let new_len = tool_input
                    .get("new_string")
                    .and_then(|v| v.as_str())
                    .map(|s| s.len())
                    .unwrap_or(0);

                if new_len > old_len * 2 {
                    pattern.operation_type = "expansion".to_string();
                    pattern.pattern_description = "Code expansion/feature addition".to_string();
                } else if old_len > 0 && new_len < old_len / 2 {
                    pattern.operation_type = "removal".to_string();
                    pattern.pattern_description = "Code removal/simplification".to_string();
                } else {
                    pattern.operation_type = "modification".to_string();
                    pattern.pattern_description = "In-place modification".to_string();
                }
            }
            "Write" => {
                pattern.operation_type = "creation".to_string();
                pattern.pattern_description = "New file creation".to_string();
            }
            _ => {
                pattern.operation_type = "modification".to_string();
                pattern.pattern_description = "Code modification".to_string();
            }
        }

        // Find WHY — look at recent user messages
        let start = edit_index.saturating_sub(5);
        for msg in &messages[start..edit_index] {
            let check_data = get_message_data(msg);
            if check_data.get("role").and_then(|v| v.as_str()) != Some("user") {
                continue;
            }
            let content_str = check_data
                .get("content")
                .map(|v| serde_json::to_string(v).unwrap_or_default())
                .unwrap_or_default();
            if content_str.len() > 50 && !content_str.contains("tool_result") {
                let truncated: String = content_str.chars().take(150).collect();
                pattern.why = truncated;
                break;
            }
        }

        return pattern;
    }

    pattern
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_edit_pattern_expansion() {
        let messages = vec![
            json!({"role": "user", "content": "Add authentication middleware to the Express app to handle JWT tokens properly"}),
            json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "name": "Edit",
                    "input": {
                        "file_path": "src/middleware/auth.ts",
                        "old_string": "pass",
                        "new_string": "const token = req.headers.authorization; verify(token); next();"
                    }
                }]
            }),
        ];
        let pattern = extract_edit_pattern(&messages, 1);
        assert_eq!(pattern.operation_type, "expansion");
        assert_eq!(pattern.file, "src/middleware/auth.ts");
        assert!(pattern.why.contains("authentication"));
    }

    #[test]
    fn test_edit_pattern_write() {
        let messages = vec![json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "name": "Write",
                "input": {"file_path": "src/new_module.rs", "content": "pub fn new() {}"}
            }]
        })];
        let pattern = extract_edit_pattern(&messages, 0);
        assert_eq!(pattern.operation_type, "creation");
        assert_eq!(pattern.file, "src/new_module.rs");
    }

    #[test]
    fn test_edit_pattern_multi_edit() {
        let edits: Vec<Value> = (0..6)
            .map(|i| json!({"old_string": format!("old{i}"), "new_string": format!("new{i}")}))
            .collect();
        let messages = vec![json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "name": "MultiEdit",
                "input": {"file_path": "src/big.rs", "edits": edits}
            }]
        })];
        let pattern = extract_edit_pattern(&messages, 0);
        assert_eq!(pattern.operation_type, "cascade_updates");
    }
}
