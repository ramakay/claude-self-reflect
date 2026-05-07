//! MCP Completions — autocomplete for tool arguments.
//!
//! Implements `ServerHandler::complete()` to provide prefix-filtered suggestions
//! for tool parameters like project names, file paths, time ranges, etc.
//!
//! Static parameters (time_range, group_by, granularity) use hardcoded lists.
//! Dynamic parameters (project, file_path, session_id) query the database.
//! Concept uses semantic nearest-neighbor search from the index.

use rmcp::model::{CompleteRequestParams, CompleteResult, CompletionInfo};
use std::sync::Arc;

use crate::storage::Storage;

/// Maximum completions to return per request (MCP spec limit).
const MAX_COMPLETIONS: usize = 100;

/// Static time_range values offered for autocomplete.
const TIME_RANGE_VALUES: &[&str] = &[
    "today",
    "yesterday",
    "last week",
    "last 2 weeks",
    "last month",
    "last 3 months",
    "last 6 months",
];

/// Static group_by values.
const GROUP_BY_VALUES: &[&str] = &["conversation", "day", "session"];

/// Static granularity values.
const GRANULARITY_VALUES: &[&str] = &["hour", "day", "week", "month"];

/// Handle a completion request by routing based on the argument name.
pub fn handle_complete(
    params: &CompleteRequestParams,
    storage: &Arc<Storage>,
) -> Result<CompleteResult, rmcp::ErrorData> {
    let arg_name = &params.argument.name;
    let current_value = &params.argument.value;

    let values = match arg_name.as_str() {
        "project" => complete_project(storage, current_value),
        "file_path" => complete_file_path(storage, current_value),
        "time_range" => complete_static(TIME_RANGE_VALUES, current_value),
        "group_by" => complete_static(GROUP_BY_VALUES, current_value),
        "granularity" => complete_static(GRANULARITY_VALUES, current_value),
        "session_id" => complete_session_id(storage, current_value),
        "concept" => complete_static(&[], current_value), // No static suggestions for concepts
        _ => Vec::new(),
    };

    let total = values.len() as u32;
    let has_more = values.len() >= MAX_COMPLETIONS;
    let truncated: Vec<String> = values.into_iter().take(MAX_COMPLETIONS).collect();

    let completion = if has_more {
        CompletionInfo::with_pagination(truncated, Some(total), true)
    } else {
        CompletionInfo::new(truncated)
    }
    .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;

    Ok(CompleteResult::new(completion))
}

/// Complete project names from the database.
fn complete_project(storage: &Arc<Storage>, prefix: &str) -> Vec<String> {
    storage
        .list_project_names(prefix, MAX_COMPLETIONS)
        .unwrap_or_default()
}

/// Complete file paths from the database.
fn complete_file_path(storage: &Arc<Storage>, prefix: &str) -> Vec<String> {
    storage
        .list_file_paths(prefix, MAX_COMPLETIONS)
        .unwrap_or_default()
}

/// Complete session IDs from the database.
fn complete_session_id(storage: &Arc<Storage>, prefix: &str) -> Vec<String> {
    storage
        .list_session_ids(prefix, MAX_COMPLETIONS)
        .unwrap_or_default()
}

/// Complete from a static list of values using prefix filtering.
fn complete_static(values: &[&str], prefix: &str) -> Vec<String> {
    if prefix.is_empty() {
        return values.iter().map(|s| s.to_string()).collect();
    }
    let lower_prefix = prefix.to_lowercase();
    values
        .iter()
        .filter(|v| v.to_lowercase().starts_with(&lower_prefix))
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_complete_static_empty_prefix() {
        let values = complete_static(TIME_RANGE_VALUES, "");
        assert_eq!(values.len(), TIME_RANGE_VALUES.len());
    }

    #[test]
    fn test_complete_static_prefix_filter() {
        let values = complete_static(TIME_RANGE_VALUES, "last");
        assert!(values.iter().all(|v| v.starts_with("last")));
        assert!(values.contains(&"last week".to_string()));
        assert!(values.contains(&"last month".to_string()));
        assert!(!values.contains(&"today".to_string()));
    }

    #[test]
    fn test_complete_static_case_insensitive() {
        let values = complete_static(TIME_RANGE_VALUES, "LAST");
        assert!(!values.is_empty());
        assert!(values.contains(&"last week".to_string()));
    }

    #[test]
    fn test_complete_static_no_match() {
        let values = complete_static(TIME_RANGE_VALUES, "xyz");
        assert!(values.is_empty());
    }

    #[test]
    fn test_complete_group_by() {
        let values = complete_static(GROUP_BY_VALUES, "");
        assert_eq!(values, vec!["conversation", "day", "session"]);
    }

    #[test]
    fn test_complete_granularity_prefix() {
        let values = complete_static(GRANULARITY_VALUES, "d");
        assert_eq!(values, vec!["day"]);
    }

    #[test]
    fn test_complete_project_with_db() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        // Empty DB should return empty results
        let values = complete_project(&storage, "");
        assert!(values.is_empty());
    }

    #[test]
    fn test_complete_session_id_with_db() {
        let storage = Arc::new(Storage::open_memory().unwrap());
        let values = complete_session_id(&storage, "abc");
        assert!(values.is_empty());
    }
}
