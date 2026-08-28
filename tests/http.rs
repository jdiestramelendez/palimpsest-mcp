//! Ticket 010 tests: streamable HTTP transport with bearer auth.

use std::sync::Arc;

use palimpsest_mcp::http::{serve_http, validate_http_binding};
use palimpsest_mcp::providers::mock::{MockEmbedder, MockExtractor};
use palimpsest_mcp::tools::MemoryServer;

const DIMS: usize = 8;

fn server() -> MemoryServer {
    let db = palimpsest_mcp::db::Db::open_in_memory(DIMS).expect("db");
    MemoryServer::new(
        db,
        Arc::new(MockEmbedder::new(DIMS)),
        Arc::new(MockExtractor::failing()),
    )
}

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}"#;

#[test]
fn non_localhost_requires_a_token() {
    let public: std::net::SocketAddr = "0.0.0.0:8321".parse().expect("addr");
    let local: std::net::SocketAddr = "127.0.0.1:8321".parse().expect("addr");
    let v6_local: std::net::SocketAddr = "[::1]:8321".parse().expect("addr");

    let err = validate_http_binding(&public, None).expect_err("must refuse");
    assert!(err.to_string().contains("PALIMPSEST_HTTP_TOKEN"), "{err}");
    validate_http_binding(&public, Some("  ")).expect_err("blank token refused");
    validate_http_binding(&public, Some("secret")).expect("token allows public bind");
    validate_http_binding(&local, None).expect("localhost needs no token");
    validate_http_binding(&v6_local, None).expect("v6 loopback needs no token");
}

async fn spawn(token: Option<&str>) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let token = token.map(String::from);
    tokio::spawn(async move {
        let _ = serve_http(server(), listener, token).await;
    });
    addr
}

async fn post_initialize(addr: std::net::SocketAddr, auth: Option<&str>) -> (u16, String) {
    let client = reqwest::Client::new();
    let mut req = client
        .post(format!("http://{addr}/mcp"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(INITIALIZE);
    if let Some(t) = auth {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let resp = req.send().await.expect("request");
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    (status, body)
}

#[tokio::test]
async fn initialize_works_over_http_and_auth_gates_it() {
    // Token-protected server.
    let addr = spawn(Some("s3cret")).await;

    let (status, _) = post_initialize(addr, None).await;
    assert_eq!(status, 401, "missing token must be rejected");
    let (status, _) = post_initialize(addr, Some("wrong")).await;
    assert_eq!(status, 401, "wrong token must be rejected");

    let (status, body) = post_initialize(addr, Some("s3cret")).await;
    assert_eq!(status, 200, "correct token accepted: {body}");
    assert!(body.contains("palimpsest-mcp"), "serverInfo present: {body}");

    // Tokenless localhost server: open.
    let open_addr = spawn(None).await;
    let (status, body) = post_initialize(open_addr, None).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("palimpsest-mcp"));
}
