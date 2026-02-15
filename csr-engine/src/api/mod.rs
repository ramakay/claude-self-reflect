//! Anthropic API client for batch narrative generation (Layer 3).
//!
//! Only activates when `ANTHROPIC_API_KEY` is set.

pub mod sanitize;
pub mod types;

use anyhow::Result;
use async_trait::async_trait;

use types::{BatchRequest, BatchResponse, BatchResultItem};

/// Trait for testable batch API interaction.
#[async_trait]
pub trait BatchClient: Send + Sync {
    /// Submit a batch of narrative requests. Returns the batch ID.
    async fn create_batch(&self, requests: Vec<BatchRequest>) -> Result<String>;

    /// Poll batch status.
    async fn get_batch_status(&self, batch_id: &str) -> Result<BatchResponse>;

    /// Retrieve completed batch results (conv_id, narrative_text).
    async fn get_batch_results(&self, batch_id: &str) -> Result<Vec<BatchResultItem>>;
}

/// Real Anthropic API client using reqwest.
pub struct AnthropicClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl AnthropicClient {
    /// Create a new client from the ANTHROPIC_API_KEY environment variable.
    /// Returns None if the key is not set.
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").ok()?;
        if api_key.is_empty() {
            return None;
        }
        Some(Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: "https://api.anthropic.com".to_string(),
        })
    }

    /// Check if the Anthropic API is configured.
    pub fn is_configured() -> bool {
        std::env::var("ANTHROPIC_API_KEY")
            .map(|k| !k.is_empty())
            .unwrap_or(false)
    }
}

#[async_trait]
impl BatchClient for AnthropicClient {
    async fn create_batch(&self, requests: Vec<BatchRequest>) -> Result<String> {
        let body = serde_json::json!({
            "requests": requests.iter().map(|r| {
                serde_json::json!({
                    "custom_id": r.custom_id,
                    "params": {
                        "model": "claude-haiku-4-5-20251001",
                        "max_tokens": 4096,
                        "messages": [
                            {"role": "user", "content": r.prompt.clone()}
                        ]
                    }
                })
            }).collect::<Vec<_>>()
        });

        let resp = self
            .client
            .post(format!("{}/v1/messages/batches", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(anyhow::anyhow!(
                "Anthropic batch create failed ({}): {}",
                status,
                text
            ));
        }

        let result: serde_json::Value = serde_json::from_str(&text)?;
        let batch_id = result
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing batch id in response"))?;

        Ok(batch_id.to_string())
    }

    async fn get_batch_status(&self, batch_id: &str) -> Result<BatchResponse> {
        let resp = self
            .client
            .get(format!(
                "{}/v1/messages/batches/{}",
                self.base_url, batch_id
            ))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(anyhow::anyhow!(
                "Anthropic batch status failed ({}): {}",
                status,
                text
            ));
        }

        let result: serde_json::Value = serde_json::from_str(&text)?;
        let processing_status = result
            .get("processing_status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        Ok(BatchResponse {
            id: batch_id.to_string(),
            processing_status,
        })
    }

    async fn get_batch_results(&self, batch_id: &str) -> Result<Vec<BatchResultItem>> {
        let resp = self
            .client
            .get(format!(
                "{}/v1/messages/batches/{}/results",
                self.base_url, batch_id
            ))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(anyhow::anyhow!(
                "Anthropic batch results failed ({}): {}",
                status,
                text
            ));
        }

        // Results come as JSONL (one JSON object per line)
        let mut results = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let parsed: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let custom_id = parsed
                .get("custom_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Extract text from the response
            let narrative = parsed
                .get("result")
                .and_then(|r| r.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
                .and_then(|arr| arr.first())
                .and_then(|item| item.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();

            if !custom_id.is_empty() && !narrative.is_empty() {
                results.push(BatchResultItem {
                    custom_id,
                    narrative: sanitize::sanitize_narrative(&narrative),
                });
            }
        }

        Ok(results)
    }
}

/// Mock batch client for testing.
#[cfg(test)]
pub struct MockBatchClient {
    pub batch_id: String,
    pub results: Vec<BatchResultItem>,
}

#[cfg(test)]
#[async_trait]
impl BatchClient for MockBatchClient {
    async fn create_batch(&self, _requests: Vec<BatchRequest>) -> Result<String> {
        Ok(self.batch_id.clone())
    }

    async fn get_batch_status(&self, batch_id: &str) -> Result<BatchResponse> {
        Ok(BatchResponse {
            id: batch_id.to_string(),
            processing_status: "ended".to_string(),
        })
    }

    async fn get_batch_results(&self, _batch_id: &str) -> Result<Vec<BatchResultItem>> {
        Ok(self.results.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_configured_without_key() {
        // Remove the key if set (test isolation)
        std::env::remove_var("ANTHROPIC_API_KEY");
        assert!(!AnthropicClient::is_configured());
    }

    #[test]
    fn test_from_env_without_key() {
        std::env::remove_var("ANTHROPIC_API_KEY");
        assert!(AnthropicClient::from_env().is_none());
    }

    #[tokio::test]
    async fn test_mock_client() {
        let mock = MockBatchClient {
            batch_id: "batch_123".to_string(),
            results: vec![BatchResultItem {
                custom_id: "conv_abc".to_string(),
                narrative: "Test narrative".to_string(),
            }],
        };
        let id = mock
            .create_batch(vec![BatchRequest {
                custom_id: "conv_abc".to_string(),
                prompt: "test".to_string(),
            }])
            .await
            .unwrap();
        assert_eq!(id, "batch_123");

        let status = mock.get_batch_status(&id).await.unwrap();
        assert_eq!(status.processing_status, "ended");

        let results = mock.get_batch_results(&id).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].custom_id, "conv_abc");
    }
}
