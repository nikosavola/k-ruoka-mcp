//! Stateless Streamable HTTP for one configured account. Authorization belongs at the proxy.

use std::sync::Arc;

use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header::ORIGIN},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio_util::sync::CancellationToken;

use super::CartServer;

#[derive(Default)]
pub struct HttpConfig {
    pub allowed_hosts: Vec<String>,
    pub allowed_origins: Vec<String>,
}

pub fn router(handler: CartServer, config: HttpConfig, cancellation: CancellationToken) -> Router {
    let mut transport = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .with_cancellation_token(cancellation);
    if !config.allowed_hosts.is_empty() {
        transport = transport.with_allowed_hosts(config.allowed_hosts);
    }
    // Clone the existing handler: a new Session per request would fight for the profile lock.
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        LocalSessionManager::default().into(),
        transport,
    );
    Router::new()
        .nest_service("/mcp", service)
        .route_layer(middleware::from_fn_with_state(
            Arc::new(config.allowed_origins),
            validate_origin,
        ))
        .route("/healthz", get(|| async { StatusCode::OK }))
}

async fn validate_origin(
    State(allowed): State<Arc<Vec<String>>>,
    request: Request,
    next: Next,
) -> Response {
    // Missing Origin is normal for MCP clients; an empty allowlist rejects browser origins.
    let mut origins = request.headers().get_all(ORIGIN).iter();
    if let Some(origin) = origins.next() {
        let valid = origin
            .to_str()
            .ok()
            .is_some_and(|origin| allowed.iter().any(|entry| entry == origin));
        if !valid || origins.next().is_some() {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    next.run(request).await
}
