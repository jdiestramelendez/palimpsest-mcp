//! Provider layer tests (Step 2). No network calls.

use palimpsest_mcp::config::{Config, EmbedderProvider, ExtractorProvider};
use palimpsest_mcp::providers::mock::MockEmbedder;
use palimpsest_mcp::providers::{from_config, Embedder};

#[tokio::test]
async fn factory_selects_ollama_by_default() {
    let config = Config::default();
    let (embedder, _extractor) = from_config(&config).expect("default factory");
    assert_eq!(embedder.dims(), 768);
}

#[test]
fn factory_reports_missing_api_key_by_name() {
    let mut config = Config::default();
    config.embedder.provider = EmbedderProvider::OpenAi;
    config.embedder.api_key_env = "MEMORY_MCP_TEST_KEY_THAT_DOES_NOT_EXIST".into();
    let err = from_config(&config).err().expect("must fail without key");
    assert!(
        err.to_string().contains("MEMORY_MCP_TEST_KEY_THAT_DOES_NOT_EXIST"),
        "error must name the env var: {err}"
    );
}

#[test]
fn factory_selects_cloud_impls_when_keys_present() {
    std::env::set_var("MEMORY_MCP_TEST_FAKE_KEY", "sk-test");
    let mut config = Config::default();
    config.embedder.provider = EmbedderProvider::OpenAi;
    config.embedder.api_key_env = "MEMORY_MCP_TEST_FAKE_KEY".into();
    config.extractor.provider = ExtractorProvider::Anthropic;
    config.extractor.api_key_env = "MEMORY_MCP_TEST_FAKE_KEY".into();
    from_config(&config).expect("cloud factory with keys set");

    config.extractor.provider = ExtractorProvider::OpenAi;
    from_config(&config).expect("openai extractor with key set");
}

#[tokio::test]
async fn mock_embedder_is_deterministic_with_correct_dims() {
    let embedder = MockEmbedder::new(64);
    let texts = vec!["the database uses WAL mode".to_string()];
    let a = embedder.embed(&texts).await.expect("embed a");
    let b = embedder.embed(&texts).await.expect("embed b");
    assert_eq!(a, b, "same text must embed identically");
    assert_eq!(a[0].len(), 64);
    assert!(a[0].iter().any(|x| *x != 0.0), "vector must be non-trivial");

    // Overlapping text embeds closer than disjoint text (cosine on
    // L2-normalized vectors = dot product).
    let others = vec![
        "the database uses journal mode".to_string(),
        "quantum entanglement of bananas".to_string(),
    ];
    let vs = embedder.embed(&others).await.expect("embed others");
    let dot = |x: &[f32], y: &[f32]| x.iter().zip(y).map(|(a, b)| a * b).sum::<f32>();
    assert!(
        dot(&a[0], &vs[0]) > dot(&a[0], &vs[1]),
        "overlapping text must be closer"
    );
}

mod serde_roundtrips {
    use palimpsest_mcp::providers::anthropic::*;
    use palimpsest_mcp::providers::ollama::*;
    use palimpsest_mcp::providers::openai::*;

    /// Serialize -> deserialize -> serialize must be stable.
    fn roundtrip<T: serde::Serialize + serde::de::DeserializeOwned>(value: &T) {
        let json = serde_json::to_value(value).expect("serialize");
        let back: T = serde_json::from_value(json.clone()).expect("deserialize");
        let json2 = serde_json::to_value(&back).expect("re-serialize");
        assert_eq!(json, json2);
    }

    #[test]
    fn ollama_types() {
        roundtrip(&OllamaEmbedRequest {
            model: "nomic-embed-text".into(),
            input: vec!["hi".into()],
        });
        roundtrip(&OllamaEmbedResponse {
            embeddings: vec![vec![0.1, 0.2]],
        });
        roundtrip(&OllamaChatRequest {
            model: "qwen2.5:7b".into(),
            messages: vec![OllamaChatMessage {
                role: "user".into(),
                content: "hello".into(),
            }],
            stream: false,
        });
        roundtrip(&OllamaChatResponse {
            message: OllamaChatMessage {
                role: "assistant".into(),
                content: "hi".into(),
            },
        });
    }

    #[test]
    fn openai_types() {
        roundtrip(&OpenAiEmbedRequest {
            model: "text-embedding-3-small".into(),
            input: vec!["hi".into()],
            dimensions: Some(1536),
        });
        roundtrip(&OpenAiEmbedResponse {
            data: vec![OpenAiEmbedItem {
                index: 0,
                embedding: vec![0.5],
            }],
        });
        roundtrip(&OpenAiChatRequest {
            model: "gpt-4o-mini".into(),
            messages: vec![OpenAiChatMessage {
                role: "system".into(),
                content: "s".into(),
            }],
        });
        roundtrip(&OpenAiChatResponse {
            choices: vec![OpenAiChatChoice {
                message: OpenAiChatMessage {
                    role: "assistant".into(),
                    content: "hi".into(),
                },
            }],
        });
    }

    #[test]
    fn anthropic_types() {
        roundtrip(&AnthropicRequest {
            model: "claude-haiku-4-5".into(),
            max_tokens: 1024,
            system: "s".into(),
            messages: vec![AnthropicMessage {
                role: "user".into(),
                content: "hello".into(),
            }],
        });
        roundtrip(&AnthropicResponse {
            content: vec![AnthropicContentBlock {
                block_type: "text".into(),
                text: "hi".into(),
            }],
        });
    }

    /// Deserialize the documented Anthropic wire shape (extra fields ignored).
    #[test]
    fn anthropic_wire_response() {
        let wire = serde_json::json!({
            "id": "msg_1", "type": "message", "role": "assistant",
            "content": [{"type": "text", "text": "extracted"}],
            "model": "claude-haiku-4-5", "stop_reason": "end_turn"
        });
        let resp: AnthropicResponse = serde_json::from_value(wire).expect("wire decode");
        assert_eq!(resp.content[0].text, "extracted");
    }
}
