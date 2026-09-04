//! MCP elicitation flows with separate authority semantics.
//!
//! When `store_reflection` receives content >2000 chars, it asks the client
//! to confirm before storing. Uses rmcp's form-based elicitation with a
//! simple { confirm: boolean } schema.
//!
//! Threshold is 2000 chars (not 500) to avoid annoying prompts on normal
//! reflections. Only truly large content (paste accidents, verbose dumps)
//! triggers the dialog.
//!
//! The reflection size guard remains non-blocking: unsupported clients proceed
//! without confirmation. Resolution elicitation is different: every failure
//! still permits the append-only write but records only `source='agent'`.

use rmcp::model::{ElicitRequestParams, ElicitResult, ElicitationAction, ElicitationSchema};
use rmcp::service::RequestContext;
use rmcp::RoleServer;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

use crate::storage::queries::{RESOLUTION_SOURCE_AGENT, RESOLUTION_SOURCE_USER_CONFIRMED};

/// Content length threshold that triggers confirmation.
/// Set to 2000 to avoid noise on normal reflections — only fires for
/// unusually large content (paste accidents, verbose log dumps).
const CONFIRMATION_THRESHOLD: usize = 2000;

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

    let params = ElicitRequestParams::FormElicitationParams {
        meta: None,
        message,
        requested_schema: schema,
    };

    let result: Result<ElicitResult, _> = context.peer.create_elicitation(params).await;

    match result {
        Ok(response) => matches!(response.action, ElicitationAction::Accept),
        Err(_) => true, // Client doesn't support elicitation — proceed without confirmation
    }
}

/// Exact ledger payload shown to the user and echoed by a confirming client.
/// Field order is part of the canonical JSON and therefore part of its digest.
#[derive(Debug)]
pub struct ResolutionConfirmationPayload {
    canonical_json: String,
    digest: String,
}

#[derive(Serialize)]
struct CanonicalResolutionPayload<'a> {
    chunk_ids: &'a [String],
    status: &'a str,
    claim: Option<&'a str>,
    evidence: &'a str,
}

impl ResolutionConfirmationPayload {
    pub fn new(chunk_ids: &[String], status: &str, claim: Option<&str>, evidence: &str) -> Self {
        let canonical_json = serde_json::to_string(&CanonicalResolutionPayload {
            chunk_ids,
            status,
            claim,
            evidence,
        })
        .expect("resolution payload serialization is infallible");
        let digest_bytes = Sha256::digest(canonical_json.as_bytes());
        let mut digest = String::with_capacity(digest_bytes.len() * 2);
        for byte in digest_bytes {
            write!(&mut digest, "{byte:02x}").expect("writing to String is infallible");
        }
        Self {
            canonical_json,
            digest,
        }
    }

    pub fn canonical_json(&self) -> &str {
        &self.canonical_json
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// Classify an elicitation response conservatively. This records a local
/// authority event for this exact resolution-ledger payload only. It is not
/// PPMF confirmation: no principal, action target, risk, or scope is bound.
fn resolution_source_from_response<E>(
    payload: &ResolutionConfirmationPayload,
    response: Result<ElicitResult, E>,
) -> &'static str {
    let Ok(response) = response else {
        return RESOLUTION_SOURCE_AGENT;
    };
    if response.action != ElicitationAction::Accept {
        return RESOLUTION_SOURCE_AGENT;
    }
    let Some(content) = response
        .content
        .as_ref()
        .and_then(|value| value.as_object())
    else {
        return RESOLUTION_SOURCE_AGENT;
    };
    let exact_match = content.get("confirm").and_then(|value| value.as_bool()) == Some(true)
        && content
            .get("canonical_payload")
            .and_then(|value| value.as_str())
            == Some(payload.canonical_json())
        && content
            .get("payload_digest")
            .and_then(|value| value.as_str())
            == Some(payload.digest());
    if exact_match {
        RESOLUTION_SOURCE_USER_CONFIRMED
    } else {
        RESOLUTION_SOURCE_AGENT
    }
}

/// Ask the client to confirm an exact resolution-ledger payload. Every failure
/// mode returns `agent`; callers still append the ledger row and never block
/// the write indefinitely.
pub async fn request_resolution_confirmation(
    payload: &ResolutionConfirmationPayload,
    context: &RequestContext<RoleServer>,
) -> &'static str {
    let canonical_json = payload.canonical_json().to_string();
    let digest = payload.digest().to_string();
    let message = format!(
        "Confirm this exact local resolution-ledger payload:\n\n{canonical_json}\n\nSHA-256: {digest}"
    );
    let schema = ElicitationSchema::builder()
        .required_bool_with("confirm", |value| value.with_default(false))
        .required_string_with("canonical_payload", |value| {
            value.with_default(canonical_json)
        })
        .required_string_with("payload_digest", |value| value.with_default(digest))
        .build();
    let Ok(schema) = schema else {
        return RESOLUTION_SOURCE_AGENT;
    };
    let params = ElicitRequestParams::FormElicitationParams {
        meta: None,
        message,
        requested_schema: schema,
    };
    let response = context
        .peer
        .create_elicitation_with_timeout(params, Some(std::time::Duration::from_secs(30)))
        .await;
    resolution_source_from_response(payload, response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_needs_confirmation_short() {
        assert!(!needs_confirmation("short content"));
        assert!(!needs_confirmation(&"a".repeat(500)));
        assert!(!needs_confirmation(&"a".repeat(2000)));
    }

    #[test]
    fn test_needs_confirmation_long() {
        assert!(needs_confirmation(&"a".repeat(2001)));
        assert!(needs_confirmation(&"a".repeat(5000)));
    }

    #[test]
    fn test_threshold_constant() {
        assert_eq!(CONFIRMATION_THRESHOLD, 2000);
    }

    fn sample_resolution_payload() -> ResolutionConfirmationPayload {
        ResolutionConfirmationPayload::new(
            &["chunk-2".into(), "chunk-1".into()],
            "resolved",
            Some("the claim"),
            "commit abc",
        )
    }

    #[test]
    fn resolution_payload_is_canonical_and_digest_bound() {
        let payload = sample_resolution_payload();
        assert_eq!(
            payload.canonical_json(),
            r#"{"chunk_ids":["chunk-2","chunk-1"],"status":"resolved","claim":"the claim","evidence":"commit abc"}"#
        );
        assert_eq!(
            payload.digest(),
            "2db459422f7fa3e1e5b669cd2b2eb33be2f75791460a56082a5629601fe23419"
        );
    }

    #[test]
    fn only_exact_accepted_resolution_payload_is_user_confirmed() {
        let payload = sample_resolution_payload();
        let accepted =
            ElicitResult::new(ElicitationAction::Accept).with_content(serde_json::json!({
                "confirm": true,
                "canonical_payload": payload.canonical_json(),
                "payload_digest": payload.digest(),
            }));

        assert_eq!(
            resolution_source_from_response(&payload, Ok::<_, &str>(accepted)),
            RESOLUTION_SOURCE_USER_CONFIRMED
        );
    }

    #[test]
    fn unsupported_client_and_payload_mismatch_stay_agent_sourced() {
        let payload = sample_resolution_payload();
        assert_eq!(
            resolution_source_from_response(
                &payload,
                Err::<ElicitResult, _>("client does not support elicitation"),
            ),
            RESOLUTION_SOURCE_AGENT
        );

        let mismatched =
            ElicitResult::new(ElicitationAction::Accept).with_content(serde_json::json!({
                "confirm": true,
                "canonical_payload": "{}",
                "payload_digest": payload.digest(),
            }));
        assert_eq!(
            resolution_source_from_response(&payload, Ok::<_, &str>(mismatched)),
            RESOLUTION_SOURCE_AGENT
        );
    }

    #[test]
    fn decline_cancel_and_incomplete_accept_stay_agent_sourced() {
        let payload = sample_resolution_payload();
        for response in [
            ElicitResult::new(ElicitationAction::Decline),
            ElicitResult::new(ElicitationAction::Cancel),
            ElicitResult::new(ElicitationAction::Accept),
        ] {
            assert_eq!(
                resolution_source_from_response(&payload, Ok::<_, &str>(response)),
                RESOLUTION_SOURCE_AGENT
            );
        }
    }
}
