//! Deterministic test doubles: a hash-based embedder and a scripted
//! extractor. Compiled unconditionally (a tiny module) so integration
//! tests in `tests/` can use them; not wired into the factory.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use async_trait::async_trait;

use super::{Embedder, Extractor};
use crate::error::{Error, Result};

/// Deterministic embedder: the vector is derived from token hashes, so the
/// same text always embeds identically and texts sharing words land closer
/// together. Tests can also pin exact vectors per text via [`MockEmbedder::pin`].
pub struct MockEmbedder {
    dims: usize,
    pinned: Mutex<Vec<(String, Vec<f32>)>>,
}

impl MockEmbedder {
    /// Create a mock embedder producing `dims`-dimensional vectors.
    pub fn new(dims: usize) -> Self {
        Self {
            dims,
            pinned: Mutex::new(Vec::new()),
        }
    }

    /// Pin an exact vector for a given text (checked before hashing).
    /// The vector is padded/truncated to `dims`.
    pub fn pin(&self, text: &str, vector: Vec<f32>) {
        let mut v = vector;
        v.resize(self.dims, 0.0);
        if let Ok(mut pinned) = self.pinned.lock() {
            pinned.push((text.to_string(), v));
        }
    }

    fn hash_embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; self.dims];
        for token in text.to_lowercase().split_whitespace() {
            let mut h = DefaultHasher::new();
            token.hash(&mut h);
            let idx = (h.finish() as usize) % self.dims;
            v[idx] += 1.0;
        }
        // L2-normalize so distances behave like real embeddings.
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }
}

#[async_trait]
impl Embedder for MockEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let pinned = self
            .pinned
            .lock()
            .map_err(|_| Error::Provider("mock embedder poisoned".into()))?;
        Ok(texts
            .iter()
            .map(|t| {
                pinned
                    .iter()
                    .find(|(k, _)| k == t)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| self.hash_embed(t))
            })
            .collect())
    }

    fn dims(&self) -> usize {
        self.dims
    }
}

/// Scripted extractor: returns canned responses in order, then errors when
/// the script is exhausted. An empty script always errors — which doubles
/// as the "extractor hard failure" fixture.
pub struct MockExtractor {
    responses: Mutex<Vec<String>>,
}

impl MockExtractor {
    /// Create a mock extractor that replies with `responses` in order.
    pub fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }

    /// An extractor that always fails.
    pub fn failing() -> Self {
        Self::new(vec![])
    }
}

#[async_trait]
impl Extractor for MockExtractor {
    async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
        let mut responses = self
            .responses
            .lock()
            .map_err(|_| Error::Provider("mock extractor poisoned".into()))?;
        if responses.is_empty() {
            return Err(Error::Provider("mock extractor: script exhausted".into()));
        }
        Ok(responses.remove(0))
    }
}
