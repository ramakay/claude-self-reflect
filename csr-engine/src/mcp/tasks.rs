//! MCP Tasks — task eligibility for heavy search operations.

/// Tools eligible for async task execution when the client declares task support.
const TASKABLE_TOOLS: &[&str] = &[
    "csr_reflect_on_past",
    "csr_search_by_concept",
    "csr_search_insights",
    "search_by_recency",
];

/// Check if a tool name is eligible for async task execution.
pub fn is_taskable(tool_name: &str) -> bool {
    TASKABLE_TOOLS.contains(&tool_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_taskable() {
        assert!(is_taskable("csr_reflect_on_past"));
        assert!(is_taskable("csr_search_by_concept"));
        assert!(is_taskable("csr_search_insights"));
        assert!(is_taskable("search_by_recency"));
        assert!(!is_taskable("store_reflection"));
        assert!(!is_taskable("csr_quick_check"));
    }
}
