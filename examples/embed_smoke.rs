//! Smoke test against a live local Ollama: `cargo run --example embed_smoke`.
//! Prints the dimensionality of one real embedding.

use memory_mcp::providers::ollama::OllamaEmbedder;
use memory_mcp::providers::Embedder;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let embedder = OllamaEmbedder::new(None, "nomic-embed-text".into(), 768);
    let vectors = embedder
        .embed(&["hello from memory-mcp".to_string()])
        .await?;
    println!("got {} vector(s), dims = {}", vectors.len(), vectors[0].len());
    Ok(())
}
