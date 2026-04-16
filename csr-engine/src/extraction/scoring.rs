//! Message importance scoring (Opus-validated weights).

use serde_json::Value;

use super::{content_to_lower, get_message_data};

/// Calculate importance score for a message using Opus-validated weights.
///
/// Priority hierarchy:
/// 1. User requests (10pts) — The "what"
/// 2. Successful edits (9pts) — The "how"
/// 3. Blocking errors (9pts) — Critical learning moments
/// 4. Solution indicators (8pts) — Confirmation
/// 5. Build success (7pts) — Validation
/// 6. Test failures (6pts) — Negative signals
/// 7. Bash commands (5pts) — Actions
/// 8. Code reads (3pts) — Intermediate steps
pub fn calculate_importance(msg: &Value, index: usize, total: usize) -> f64 {
    let mut score = 0.0_f64;
    let msg_data = get_message_data(msg);
    let content = content_to_lower(&msg_data);

    // User requests (the problem) — HIGHEST PRIORITY
    if msg_data.get("role").and_then(|v| v.as_str()) == Some("user") {
        let is_tool_result = ["tool_result", "tool_use_id", "is_error"]
            .iter()
            .any(|kw| content.contains(kw));
        if !is_tool_result && content.len() > 50 {
            score += 10.0;
        }
    }

    // Tool use scoring
    if let Some(items) = msg_data.get("content").and_then(|v| v.as_array()) {
        for item in items {
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
                score += 9.0;
            } else if tool_name.contains("bash") {
                score += 5.0;
            } else if tool_name.contains("read") {
                score += 3.0;
            }
        }
    }

    // Blocking errors
    let error_keywords = [
        "error",
        "exception",
        "traceback",
        "failed",
        "failure",
        "err_",
    ];
    if error_keywords.iter().any(|kw| content.contains(kw)) {
        score += 9.0;
    }

    // Build/test success
    if content.contains("compiled successfully")
        || (content.contains("build") && content.contains("success"))
    {
        score += 7.0;
    }

    // Test failures
    if content.contains("test") && (content.contains("failed") || content.contains("error")) {
        score += 6.0;
    }

    // Solution indicators
    let solution_keywords = ["fixed", "solved", "working", "success", "completed"];
    if solution_keywords.iter().any(|kw| content.contains(kw)) {
        score += 8.0;
    }

    // Position bias — beginnings and ends are often important
    let relative_pos = index as f64 / total.max(1) as f64;
    if !(0.1..=0.8).contains(&relative_pos) {
        score *= 1.1;
    }

    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_user_request_high_score() {
        let msg = json!({"role": "user", "content": "Please fix the authentication bug in the login flow that causes session timeout issues"});
        let score = calculate_importance(&msg, 0, 10);
        assert!(
            score >= 10.0,
            "User request should score >= 10, got {score}"
        );
    }

    #[test]
    fn test_edit_tool_high_score() {
        let msg = json!({
            "role": "assistant",
            "content": [{"type": "tool_use", "name": "Edit", "input": {"file_path": "src/main.rs"}}]
        });
        let score = calculate_importance(&msg, 5, 10);
        assert!(score >= 9.0, "Edit should score >= 9, got {score}");
    }

    #[test]
    fn test_tool_result_noise_low_score() {
        let msg = json!({"role": "user", "content": "tool_result tool_use_id result data"});
        let score = calculate_importance(&msg, 5, 10);
        assert!(
            score < 10.0,
            "Tool result noise should score < 10, got {score}"
        );
    }

    #[test]
    fn test_error_message_scores() {
        let msg = json!({"role": "assistant", "content": "error: cannot find module 'auth'"});
        let score = calculate_importance(&msg, 5, 10);
        assert!(score >= 9.0, "Error message should score >= 9, got {score}");
    }

    #[test]
    fn test_solution_indicator_scores() {
        let msg = json!({"role": "assistant", "content": "The bug has been fixed and the build compiled successfully."});
        let score = calculate_importance(&msg, 9, 10);
        // "fixed" (8) + "compiled successfully" (7) + position bias
        assert!(score >= 15.0, "Solution should score high, got {score}");
    }
}
