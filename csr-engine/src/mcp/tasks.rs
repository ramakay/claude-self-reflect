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

/// Opt-in switch for the MCP task path (`CSR_MCP_TASKS=1`).
///
/// Off by default since 2026-10-25: Claude Code 2.1.308+ declares the
/// `io.modelcontextprotocol/tasks` extension at `initialize` but never
/// follows a `resultType: "task"` reply with `tasks/get`, so every taskable
/// tool showed an empty result from 2026-10-22 on (44 calls across 144
/// transcripts). These searches finish in 4-900 ms; the task handle buys
/// nothing until a client is seen polling.
pub fn tasks_enabled() -> bool {
    std::env::var("CSR_MCP_TASKS").is_ok_and(|v| v == "1")
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

    #[test]
    fn tasks_are_off_unless_opted_in() {
        // Default: the env var is unset in the test process, so the task path
        // stays closed. The opt-in value is checked in-process without
        // mutating the environment (set_var is unsound under threads).
        if std::env::var_os("CSR_MCP_TASKS").is_none() {
            assert!(!tasks_enabled());
        }
        assert!(
            std::env::var("CSR_MCP_TASKS")
                .map(|v| v == "1")
                .unwrap_or(false)
                == tasks_enabled()
        );
    }
}
