//! Anthropic extractor backend via the `/v1/messages` API.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{http, Extractor};
use crate::error::{Error, Result};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_TOKENS: u32 = 4096;

/// Request body for `/v1/messages`.
#[derive(Debug, Serialize, Deserialize)]
pub struct AnthropicRequest {
    /// Model name, e.g. `claude-haiku-4-5`.
    pub model: String,
    /// Maximum tokens to generate.
    pub max_tokens: u32,
    /// System prompt.
    pub system: String,
    /// User messages.
    pub messages: Vec<AnthropicMessage>,
}

/// A single message.
#[derive(Debug, Serialize, Deserialize)]
pub struct AnthropicMessage {
    /// `user` or `assistant`.
    pub role: String,
    /// Message text.
    pub content: String,
}

/// Response body for `/v1/messages`.
#[derive(Debug, Serialize, Deserialize)]
pub struct AnthropicResponse {
    /// Content blocks; text blocks are concatenated.
    pub content: Vec<AnthropicContentBlock>,
}

/// One content block in a messages response.
#[derive(Debug, Serialize, Deserialize)]
pub struct AnthropicContentBlock {
    /// Block type, e.g. `text`.
    #[serde(rename = "type")]
    pub block_type: String,
    /// Text payload for `text` blocks.
    #[serde(default)]
    pub text: String,
}

/// Runs completions via the Anthropic Messages API.
pub struct AnthropicExtractor {
    client: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
}

impl AnthropicExtractor {
    /// Create an extractor; `api_key` comes from the env var named in config.
    pub fn new(base_url: Option<String>, model: String, api_key: String) -> Self {
        Self {
            client: http::client(),
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            model,
            api_key,
        }
    }
}

#[async_trait]
impl Extractor for AnthropicExtractor {
    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        let url = format!("{}/v1/messages", self.base_url);
        let body = serde_json::to_value(AnthropicRequest {
            model: self.model.clone(),
            max_tokens: MAX_TOKENS,
            system: system.into(),
            messages: vec![AnthropicMessage {
                role: "user".into(),
                content: user.into(),
            }],
        })
        .map_err(|e| Error::Provider(e.to_string()))?;
        let resp: AnthropicResponse = http::post_json(
            &self.client,
            &url,
            &[
                ("x-api-key", &self.api_key),
                ("anthropic-version", ANTHROPIC_VERSION),
            ],
            &body,
        )
        .await?;
        let text: String = resp
            .content
            .iter()
            .filter(|b| b.block_type == "text")
            .map(|b| b.text.as_str())
            .collect();
        if text.is_empty() {
            return Err(Error::Provider("anthropic returned no text content".into()));
        }
        Ok(text)
    }
}
