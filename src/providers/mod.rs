//! Embedding and extraction providers.
//!
//! Two object-safe async traits ([`Embedder`], [`Extractor`]) with local
//! Ollama implementations as the keyless default, plus OpenAI and Anthropic
//! cloud backends. The [`from_config`] factory picks implementations from
//! [`Config`]. Deterministic mocks live in [`mock`] for pipeline tests.

pub mod anthropic;
pub mod http;
pub mod mock;
pub mod ollama;
pub mod openai;

use std::sync::Arc;

use async_trait::async_trait;

use crate::config::{Config, EmbedderProvider, ExtractorProvider};
use crate::error::Result;

/// Turns text into fixed-dimensionality embedding vectors.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Embed each input text; returns one vector per input, in order.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Dimensionality of the vectors this embedder produces.
    fn dims(&self) -> usize;
}

/// LLM completion backend used by the extraction pipeline.
#[async_trait]
pub trait Extractor: Send + Sync {
    /// Run a single system+user completion and return the raw text reply.
    /// The extraction pipeline parses JSON out of this.
    async fn complete(&self, system: &str, user: &str) -> Result<String>;
}

/// Build the configured embedder + extractor pair.
///
/// Cloud providers read their API key from the environment variable named in
/// the config; a missing key is reported with the variable name.
pub fn from_config(config: &Config) -> Result<(Arc<dyn Embedder>, Arc<dyn Extractor>)> {
    let embedder: Arc<dyn Embedder> = match config.embedder.provider {
        EmbedderProvider::Ollama => Arc::new(ollama::OllamaEmbedder::new(
            config.embedder.base_url.clone(),
            config.embedder.model.clone(),
            config.embedder.dims,
        )),
        EmbedderProvider::OpenAi => Arc::new(openai::OpenAiEmbedder::new(
            config.embedder.base_url.clone(),
            config.embedder.model.clone(),
            config.embedder.dims,
            Config::api_key(&config.embedder.api_key_env)?,
        )),
    };
    let extractor: Arc<dyn Extractor> = match config.extractor.provider {
        ExtractorProvider::Ollama => Arc::new(ollama::OllamaExtractor::new(
            config.extractor.base_url.clone(),
            config.extractor.model.clone(),
        )),
        ExtractorProvider::Anthropic => Arc::new(anthropic::AnthropicExtractor::new(
            config.extractor.base_url.clone(),
            config.extractor.model.clone(),
            Config::api_key(&config.extractor.api_key_env)?,
        )),
        ExtractorProvider::OpenAi => Arc::new(openai::OpenAiExtractor::new(
            config.extractor.base_url.clone(),
            config.extractor.model.clone(),
            Config::api_key(&config.extractor.api_key_env)?,
        )),
    };
    Ok((embedder, extractor))
}
