//! Local Ollama provider (default): no auth, plain HTTP to localhost.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{http, Embedder, Extractor};
use crate::error::{Error, Result};

const DEFAULT_BASE_URL: &str = "http://localhost:11434";

/// Request body for Ollama `/api/embed`.
#[derive(Debug, Serialize, Deserialize)]
pub struct OllamaEmbedRequest {
    /// Model name, e.g. `nomic-embed-text`.
    pub model: String,
    /// Batch of input texts.
    pub input: Vec<String>,
}

/// Response body for Ollama `/api/embed`.
#[derive(Debug, Serialize, Deserialize)]
pub struct OllamaEmbedResponse {
    /// One embedding per input, in order.
    pub embeddings: Vec<Vec<f32>>,
}

/// Embeds text via a local Ollama server.
pub struct OllamaEmbedder {
    client: reqwest::Client,
    base_url: String,
    model: String,
    dims: usize,
}

impl OllamaEmbedder {
    /// Create an embedder against `base_url` (default `http://localhost:11434`).
    pub fn new(base_url: Option<String>, model: String, dims: usize) -> Self {
        Self {
            client: http::client(),
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            model,
            dims,
        }
    }
}

#[async_trait]
impl Embedder for OllamaEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let url = format!("{}/api/embed", self.base_url);
        let body = serde_json::to_value(OllamaEmbedRequest {
            model: self.model.clone(),
            input: texts.to_vec(),
        })
        .map_err(|e| Error::Provider(e.to_string()))?;
        let resp: OllamaEmbedResponse = http::post_json(&self.client, &url, &[], &body).await?;
        if resp.embeddings.len() != texts.len() {
            return Err(Error::Provider(format!(
                "ollama returned {} embeddings for {} inputs",
                resp.embeddings.len(),
                texts.len()
            )));
        }
        Ok(resp.embeddings)
    }

    fn dims(&self) -> usize {
        self.dims
    }
}

/// Request body for Ollama `/api/chat`.
#[derive(Debug, Serialize, Deserialize)]
pub struct OllamaChatRequest {
    /// Model name, e.g. `qwen2.5:7b`.
    pub model: String,
    /// Chat messages (system + user).
    pub messages: Vec<OllamaChatMessage>,
    /// Non-streaming for a single JSON response.
    pub stream: bool,
}

/// A single chat message.
#[derive(Debug, Serialize, Deserialize)]
pub struct OllamaChatMessage {
    /// `system`, `user` or `assistant`.
    pub role: String,
    /// Message text.
    pub content: String,
}

/// Response body for Ollama `/api/chat` (non-streaming).
#[derive(Debug, Serialize, Deserialize)]
pub struct OllamaChatResponse {
    /// The assistant reply.
    pub message: OllamaChatMessage,
}

/// Runs completions via a local Ollama server.
pub struct OllamaExtractor {
    client: reqwest::Client,
    base_url: String,
    model: String,
}

impl OllamaExtractor {
    /// Create an extractor against `base_url` (default `http://localhost:11434`).
    pub fn new(base_url: Option<String>, model: String) -> Self {
        Self {
            client: http::client(),
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            model,
        }
    }
}

#[async_trait]
impl Extractor for OllamaExtractor {
    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        let url = format!("{}/api/chat", self.base_url);
        let body = serde_json::to_value(OllamaChatRequest {
            model: self.model.clone(),
            messages: vec![
                OllamaChatMessage {
                    role: "system".into(),
                    content: system.into(),
                },
                OllamaChatMessage {
                    role: "user".into(),
                    content: user.into(),
                },
            ],
            stream: false,
        })
        .map_err(|e| Error::Provider(e.to_string()))?;
        let resp: OllamaChatResponse = http::post_json(&self.client, &url, &[], &body).await?;
        Ok(resp.message.content)
    }
}
