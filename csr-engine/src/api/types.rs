//! Types for the Anthropic Batch API.

use serde::{Deserialize, Serialize};

/// A single request in a batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRequest {
    pub custom_id: String,
    pub prompt: String,
}

/// Batch status response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResponse {
    pub id: String,
    pub processing_status: String,
}

/// A single result item from a completed batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResultItem {
    pub custom_id: String,
    pub narrative: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_batch_request_serde() {
        let req = BatchRequest {
            custom_id: "conv_123".to_string(),
            prompt: "Analyze this conversation".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: BatchRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.custom_id, "conv_123");
    }

    #[test]
    fn test_batch_response_serde() {
        let resp = BatchResponse {
            id: "batch_abc".to_string(),
            processing_status: "in_progress".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: BatchResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.processing_status, "in_progress");
    }

    #[test]
    fn test_result_item_serde() {
        let item = BatchResultItem {
            custom_id: "conv_456".to_string(),
            narrative: "This session fixed a bug".to_string(),
        };
        let json = serde_json::to_string(&item).unwrap();
        let parsed: BatchResultItem = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.narrative, "This session fixed a bug");
    }
}
