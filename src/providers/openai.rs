//! OpenAI providers: embeddings (`text-embedding-3-small`) and chat
//! completions (`gpt-4o-mini`) as an extractor backend.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{http, Embedder, Extractor};
use crate::error::{Error, Result};

const DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// Request body for `/v1/embeddings`.
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenAiEmbedRequest {
    /// Model name, e.g. `text-embedding-3-small`.
    pub model: String,
    /// Batch of input texts.
    pub input: Vec<String>,
    /// Requested output dimensionality (supported by v3 embedding models).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<usize>,
}

/// Response body for `/v1/embeddings`.
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenAiEmbedResponse {
    /// Embedding items, ordered by `index`.
    pub data: Vec<OpenAiEmbedItem>,
}

/// One embedding in an embeddings response.
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenAiEmbedItem {
    /// Position of the corresponding input.
    pub index: usize,
    /// The vector.
    pub embedding: Vec<f32>,
}

/// Embeds text via the OpenAI embeddings API.
pub struct OpenAiEmbedder {
    client: reqwest::Client,
    base_url: String,
    model: String,
    dims: usize,
    api_key: String,
}

impl OpenAiEmbedder {
    /// Create an embedder; `api_key` comes from the env var named in config.
    pub fn new(base_url: Option<String>, model: String, dims: usize, api_key: String) -> Self {
        Self {
            client: http::client(),
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            model,
            dims,
            api_key,
        }
    }
}

#[async_trait]
impl Embedder for OpenAiEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let url = format!("{}/v1/embeddings", self.base_url);
        let body = serde_json::to_value(OpenAiEmbedRequest {
            model: self.model.clone(),
            input: texts.to_vec(),
            dimensions: Some(self.dims),
        })
        .map_err(|e| Error::Provider(e.to_string()))?;
        let auth = format!("Bearer {}", self.api_key);
        let resp: OpenAiEmbedResponse =
            http::post_json(&self.client, &url, &[("Authorization", &auth)], &body).await?;
        let mut items = resp.data;
        items.sort_by_key(|d| d.index);
        if items.len() != texts.len() {
            return Err(Error::Provider(format!(
                "openai returned {} embeddings for {} inputs",
                items.len(),
                texts.len()
            )));
        }
        Ok(items.into_iter().map(|d| d.embedding).collect())
    }

    fn dims(&self) -> usize {
        self.dims
    }
}

/// Request body for `/v1/chat/completions`.
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenAiChatRequest {
    /// Model name, e.g. `gpt-4o-mini`.
    pub model: String,
    /// Chat messages (system + user).
    pub messages: Vec<OpenAiChatMessage>,
}

/// A single chat message.
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenAiChatMessage {
    /// `system`, `user` or `assistant`.
    pub role: String,
    /// Message text.
    pub content: String,
}

/// Response body for `/v1/chat/completions`.
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenAiChatResponse {
    /// Completion choices; the first is used.
    pub choices: Vec<OpenAiChatChoice>,
}

/// One completion choice.
#[derive(Debug, Serialize, Deserialize)]
pub struct OpenAiChatChoice {
    /// The assistant reply.
    pub message: OpenAiChatMessage,
}

/// Runs completions via the OpenAI chat API.
pub struct OpenAiExtractor {
    client: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
}

impl OpenAiExtractor {
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
impl Extractor for OpenAiExtractor {
    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let body = serde_json::to_value(OpenAiChatRequest {
            model: self.model.clone(),
            messages: vec![
                OpenAiChatMessage {
                    role: "system".into(),
                    content: system.into(),
                },
                OpenAiChatMessage {
                    role: "user".into(),
                    content: user.into(),
                },
            ],
        })
        .map_err(|e| Error::Provider(e.to_string()))?;
        let auth = format!("Bearer {}", self.api_key);
        let resp: OpenAiChatResponse =
            http::post_json(&self.client, &url, &[("Authorization", &auth)], &body).await?;
        resp.choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| Error::Provider("openai returned no choices".into()))
    }
}
