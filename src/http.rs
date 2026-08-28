//! Streamable HTTP transport (opt-in via `--http`): serves the same tool
//! set as stdio at `/mcp`, with mandatory bearer-token auth on any
//! non-localhost bind. TLS is intentionally out of scope — put a reverse
//! proxy (caddy, nginx) in front for anything beyond localhost/LAN.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::{
    StreamableHttpServerConfig, StreamableHttpService,
};

use crate::error::{Error, Result};
use crate::tools::MemoryServer;

/// Environment variable holding the bearer token.
pub const TOKEN_ENV: &str = "PALIMPSEST_HTTP_TOKEN";

/// Refuse unauthenticated exposure beyond the local machine: binding a
/// non-loopback address requires a bearer token.
pub fn validate_http_binding(addr: &SocketAddr, token: Option<&str>) -> Result<()> {
    let token_ok = token.map(|t| !t.trim().is_empty()).unwrap_or(false);
    if !addr.ip().is_loopback() && !token_ok {
        return Err(Error::Config(format!(
            "refusing to serve HTTP on non-localhost {addr} without authentication. \
             Set {TOKEN_ENV} to a strong random token, or bind 127.0.0.1 instead."
        )));
    }
    Ok(())
}

/// Bearer-token gate applied to every request when a token is configured.
async fn require_bearer(
    State(token): State<Arc<String>>,
    req: Request,
    next: Next,
) -> std::result::Result<Response, StatusCode> {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match presented {
        Some(t) if t == token.as_str() => Ok(next.run(req).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

/// Serve the MCP tool set over streamable HTTP on an already-bound
/// listener. Runs until the process is stopped.
pub async fn serve_http(
    server: MemoryServer,
    listener: tokio::net::TcpListener,
    token: Option<String>,
) -> Result<()> {
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let mut app = axum::Router::new().nest_service("/mcp", service);
    if let Some(token) = token {
        app = app.layer(axum::middleware::from_fn_with_state(
            Arc::new(token),
            require_bearer,
        ));
    }
    axum::serve(listener, app)
        .await
        .map_err(|e| Error::Config(format!("http server failed: {e}")))
}
