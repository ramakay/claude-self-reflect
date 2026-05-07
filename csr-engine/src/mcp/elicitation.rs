//! MCP Elicitation — confirmation flow for store_reflection.
//!
//! When `store_reflection` receives content >500 chars, it asks the client
//! to confirm before storing. Uses rmcp's form-based elicitation with a
//! simple { confirm: boolean } schema.
//!
//! Graceful fallback: if the client doesn't support elicitation, proceeds
//! without confirmation (the tool still works, just no guard rail).

use rmcp::model::{
    CreateElicitationRequestParams, CreateElicitationResult, ElicitationAction, ElicitationSchema,
};
use rmcp::service::RequestContext;
use rmcp::RoleServer;

/// Content length threshold that triggers confirmation.
const CONFIRMATION_THRESHOLD: usize = 500;

/// Check if content should trigger a confirmation dialog.
pub fn needs_confirmation(content: &str) -> bool {
    content.len() > CONFIRMATION_THRESHOLD
}

/// Request confirmation from the client for a large reflection.
/// Returns `true` if confirmed (or if client doesn't support elicitation).
/// Returns `false` if the user declined or cancelled.
pub async fn request_confirmation(content: &str, context: &RequestContext<RoleServer>) -> bool {
    let char_count = content.chars().count();
    let preview: String = content.chars().take(100).collect();
    let message = format!(
        "Store reflection ({} chars)?\n\nPreview: {}...",
        char_count, preview
    );

    let schema = ElicitationSchema::builder()
        .required_bool("confirm")
        .optional_string("tags")
        .build();

    let schema = match schema {
        Ok(s) => s,
        Err(_) => return true, // Schema build failed, proceed without confirmation
    };

    let params = CreateElicitationRequestParams::FormElicitationParams {
        meta: None,
        message,
        requested_schema: schema,
    };

    let result: Result<CreateElicitationResult, _> = context.peer.create_elicitation(params).await;

    match result {
        Ok(response) => matches!(response.action, ElicitationAction::Accept),
        Err(_) => true, // Client doesn't support elicitation — proceed without confirmation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_needs_confirmation_short() {
        assert!(!needs_confirmation("short content"));
        assert!(!needs_confirmation(&"a".repeat(500)));
    }

    #[test]
    fn test_needs_confirmation_long() {
        assert!(needs_confirmation(&"a".repeat(501)));
        assert!(needs_confirmation(&"a".repeat(1000)));
    }

    #[test]
    fn test_threshold_constant() {
        assert_eq!(CONFIRMATION_THRESHOLD, 500);
    }
}
