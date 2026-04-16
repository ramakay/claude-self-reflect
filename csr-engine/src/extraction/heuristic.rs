//! Layer 1: Lightweight heuristic extraction.
//!
//! Runs inline during import. Only extracts HIGH confidence factual data.
//! Cost: FREE, Time: ~1ms per conversation.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::get_message_data;

/// Result of lightweight heuristic extraction (Layer 1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeuristicResult {
    pub files_mentioned: Vec<String>,
    pub tools_used: Vec<String>,
    pub has_errors: bool,
    pub message_count: usize,
    pub user_message_count: usize,
}

/// Fast heuristic extraction — runs inline during import.
/// Only extracts HIGH confidence factual data.
pub fn extract_heuristic(messages: &[Value]) -> HeuristicResult {
    let mut files_mentioned = Vec::new();
    let mut tools_used = Vec::new();
    let mut has_errors = false;
    let mut user_message_count = 0;

    for msg in messages {
        let msg_data = get_message_data(msg);

        // Count user messages
        if msg_data.get("role").and_then(|v| v.as_str()) == Some("user") {
            user_message_count += 1;
        }

        // Extract tools and files from tool_use blocks
        if let Some(content) = msg_data.get("content").and_then(|v| v.as_array()) {
            for item in content {
                if item.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
                    continue;
                }

                // Tool name
                if let Some(name) = item.get("name").and_then(|v| v.as_str()) {
                    if !tools_used.contains(&name.to_string()) {
                        tools_used.push(name.to_string());
                    }
                }

                // File path from tool input
                if let Some(fp) = item
                    .get("input")
                    .and_then(|v| v.get("file_path"))
                    .and_then(|v| v.as_str())
                {
                    if !files_mentioned.contains(&fp.to_string()) {
                        files_mentioned.push(fp.to_string());
                    }
                }
            }
        }

        // Check for errors
        if !has_errors {
            let content_str = super::content_to_lower(&msg_data);
            if content_str.contains("error") || content_str.contains("exception") {
                has_errors = true;
            }
        }
    }

    // Limit to avoid huge lists
    files_mentioned.truncate(20);
    tools_used.truncate(15);

    HeuristicResult {
        files_mentioned,
        tools_used,
        has_errors,
        message_count: messages.len(),
        user_message_count,
    }
}

/// Format a heuristic result as a searchable reflection string.
pub fn format_as_reflection(result: &HeuristicResult, project: &str) -> String {
    let mut parts = Vec::new();

    parts.push(format!("[Heuristic] Project: {project}"));
    parts.push(format!(
        "Messages: {} ({} user)",
        result.message_count, result.user_message_count
    ));

    if !result.tools_used.is_empty() {
        parts.push(format!("Tools: {}", result.tools_used.join(", ")));
    }

    if !result.files_mentioned.is_empty() {
        // Show just filenames for searchability
        let short_files: Vec<&str> = result
            .files_mentioned
            .iter()
            .take(10)
            .map(|f| f.rsplit('/').next().unwrap_or(f.as_str()))
            .collect();
        parts.push(format!("Files: {}", short_files.join(", ")));
    }

    if result.has_errors {
        parts.push("Had errors: yes".to_string());
    }

    parts.join("\n")
}

/// Shared heuristic enrichment logic — used by both Engine and FileWatcher (D-12).
/// Parses messages, runs heuristic extraction, embeds, and stores as reflection.
pub async fn enrich_conversation(
    file_path: &std::path::Path,
    conv_id: &str,
    project_name: &str,
    storage: &crate::storage::Storage,
    embeddings: &std::sync::Arc<crate::embeddings::EmbeddingEngine>,
    search: &tokio::sync::RwLock<crate::search::SearchEngine>,
) -> anyhow::Result<()> {
    let messages = crate::import::parse_jsonl_messages(file_path)?;
    if messages.is_empty() {
        return Ok(());
    }

    let result = extract_heuristic(&messages);
    let content = format_as_reflection(&result, project_name);

    let content_clone = content.clone();
    let emb = embeddings.clone();
    let embeddings_vec =
        tokio::task::spawn_blocking(move || emb.embed(&[content_clone.as_str()])).await??;

    if let Some(embedding) = embeddings_vec.into_iter().next() {
        let reflection_id = format!("heuristic_{conv_id}");
        let tags = vec!["narrative_heuristic".to_string(), format!("conv_{conv_id}")];

        storage.insert_reflection(&reflection_id, &content, &tags, &embedding)?;
        let mut idx = search.write().await;
        idx.insert_reflection(reflection_id.clone(), embedding);

        storage.mark_enrichment_completed(conv_id, "heuristic", &reflection_id)?;

        tracing::debug!(conv = %conv_id, "Layer 1 heuristic enrichment complete");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_extracts_files() {
        let messages = vec![json!({
            "role": "assistant",
            "content": [
                {"type": "tool_use", "name": "Edit", "input": {"file_path": "src/main.rs"}},
                {"type": "tool_use", "name": "Read", "input": {"file_path": "src/lib.rs"}}
            ]
        })];
        let result = extract_heuristic(&messages);
        assert!(result.files_mentioned.contains(&"src/main.rs".to_string()));
        assert!(result.files_mentioned.contains(&"src/lib.rs".to_string()));
    }

    #[test]
    fn test_extracts_tools() {
        let messages = vec![json!({
            "role": "assistant",
            "content": [
                {"type": "tool_use", "name": "Edit", "input": {}},
                {"type": "tool_use", "name": "Bash", "input": {}}
            ]
        })];
        let result = extract_heuristic(&messages);
        assert!(result.tools_used.contains(&"Edit".to_string()));
        assert!(result.tools_used.contains(&"Bash".to_string()));
    }

    #[test]
    fn test_detects_errors() {
        let messages = vec![json!({"role": "assistant", "content": "error: cannot find module"})];
        let result = extract_heuristic(&messages);
        assert!(result.has_errors);
    }

    #[test]
    fn test_formats_reflection() {
        let result = HeuristicResult {
            files_mentioned: vec!["src/main.rs".to_string()],
            tools_used: vec!["Edit".to_string(), "Bash".to_string()],
            has_errors: true,
            message_count: 10,
            user_message_count: 3,
        };
        let reflection = format_as_reflection(&result, "my-project");
        assert!(reflection.contains("[Heuristic]"));
        assert!(reflection.contains("my-project"));
        assert!(reflection.contains("Edit, Bash"));
        assert!(reflection.contains("main.rs"));
        assert!(reflection.contains("Had errors: yes"));
    }
}
