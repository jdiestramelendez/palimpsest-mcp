//! Configuration loading for memory-mcp.
//!
//! Configuration lives at `~/.memory-mcp/config.toml`. Every field has a
//! default tuned for a local, keyless Ollama setup so the server works out
//! of the box. API keys are never stored in the file — the config names an
//! environment variable and the key is read from the process environment.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Which embedding backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmbedderProvider {
    /// Local Ollama HTTP API (default, no auth).
    Ollama,
    /// OpenAI embeddings API.
    OpenAi,
}

/// Which extraction (chat/completion) backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtractorProvider {
    /// Local Ollama HTTP API (default, no auth).
    Ollama,
    /// Anthropic Messages API.
    Anthropic,
    /// OpenAI Chat Completions API.
    OpenAi,
}

/// Embedder section of config.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmbedderConfig {
    /// Backend to use.
    pub provider: EmbedderProvider,
    /// Model name, e.g. `nomic-embed-text` or `text-embedding-3-small`.
    pub model: String,
    /// Embedding dimensionality. Pinned in the DB at first creation.
    pub dims: usize,
    /// Environment variable that holds the API key (cloud providers only).
    pub api_key_env: String,
    /// Base URL override (mainly for Ollama or proxies).
    pub base_url: Option<String>,
}

impl Default for EmbedderConfig {
    fn default() -> Self {
        Self {
            provider: EmbedderProvider::Ollama,
            model: "nomic-embed-text".to_string(),
            dims: 768,
            api_key_env: "OPENAI_API_KEY".to_string(),
            base_url: None,
        }
    }
}

/// Extractor section of config.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExtractorConfig {
    /// Backend to use.
    pub provider: ExtractorProvider,
    /// Model name, e.g. `qwen2.5:7b`, `claude-haiku-4-5`, `gpt-4o-mini`.
    pub model: String,
    /// Environment variable that holds the API key (cloud providers only).
    pub api_key_env: String,
    /// Base URL override (mainly for Ollama or proxies).
    pub base_url: Option<String>,
}

impl Default for ExtractorConfig {
    fn default() -> Self {
        Self {
            provider: ExtractorProvider::Ollama,
            model: "qwen2.5:7b".to_string(),
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
            base_url: None,
        }
    }
}

/// Top-level configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Embedding backend configuration.
    pub embedder: EmbedderConfig,
    /// Extraction backend configuration.
    pub extractor: ExtractorConfig,
    /// Database path override; defaults to `~/.memory-mcp/memory.db`.
    pub db_path: Option<PathBuf>,
}

impl Config {
    /// Directory holding config and the default database: `~/.memory-mcp`.
    pub fn data_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".memory-mcp")
    }

    /// Load config from `~/.memory-mcp/config.toml`, falling back to
    /// defaults when the file does not exist.
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::data_dir().join("config.toml"))
    }

    /// Load config from an explicit path; missing file means defaults.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)?;
        toml::from_str(&raw).map_err(|e| Error::Config(format!("{}: {e}", path.display())))
    }

    /// Resolve the database path: CLI override > config file > default.
    pub fn resolve_db_path(&self, cli_override: Option<&Path>) -> PathBuf {
        if let Some(p) = cli_override {
            return p.to_path_buf();
        }
        if let Some(p) = &self.db_path {
            return p.clone();
        }
        Self::data_dir().join("memory.db")
    }

    /// Read the API key for the given provider config from the environment.
    /// Returns an actionable error naming the missing variable.
    pub fn api_key(env_var: &str) -> Result<String> {
        std::env::var(env_var).map_err(|_| {
            Error::Config(format!(
                "environment variable {env_var} is not set; export it or switch the provider to 'ollama'"
            ))
        })
    }
}
