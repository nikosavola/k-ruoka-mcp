//! Real Streamable HTTP transport against the existing fake K-Ruoka API.

mod support;

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use k_ruoka_mcp::mcp::{
    CartServer,
    http::{HttpConfig, router},
};
use rmcp::{ServiceExt, transport::StreamableHttpClientTransport};
use serde_json::{Value, json};
use support::{BANANA, MockApi, STORE, call_tool};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;

fn app(api: &MockApi, config: HttpConfig) -> Router {
    router(
        CartServer::new(Arc::new(api.clone())),
        config,
        CancellationToken::new(),
    )
}

fn rpc(body: Value) -> Request<Body> {
    Request::post("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-06-18")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn request(app: &Router, body: Value) -> Value {
    let response = app.clone().oneshot(rpc(body)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "application/json");
    assert!(!response.headers().contains_key("mcp-session-id"));
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn tool(name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": name, "arguments": arguments}})
}

#[tokio::test]
async fn initialize_and_list_tools_are_stateless_and_do_not_touch_the_browser() {
    let api = MockApi::new();
    let app = app(&api, HttpConfig::default());
    let initialized = request(
        &app,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "http-test", "version": "0"}}}),
    )
    .await;
    assert_eq!(initialized["result"]["serverInfo"]["name"], "k-ruoka-mcp");
    let listed = request(
        &app,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    let (stdio, _) = support::connect(MockApi::new()).await.unwrap();
    let tools = stdio.list_all_tools().await.unwrap();
    assert_eq!(
        listed["result"]["tools"],
        serde_json::to_value(tools).unwrap()
    );
    assert!(api.calls().is_empty());
    stdio.cancel().await.unwrap();
}

#[tokio::test]
async fn separate_requests_share_defaults_and_cart_mutations() {
    let api = MockApi::new();
    let app = app(&api, HttpConfig::default());
    let default = request(&app, tool("set_default_store", json!({"store_id": STORE}))).await;
    assert_ne!(default["result"]["isError"], true);
    let added = request(
        &app,
        tool("add_to_cart", json!({"ean": BANANA, "quantity": 2})),
    )
    .await;
    assert_ne!(added["result"]["isError"], true);
    assert_eq!(api.items(), [(BANANA.to_string(), 2.0, "kpl".to_string())]);
    assert_eq!(api.events(), ["ADD-ITEM"]);
    let cart = request(&app, tool("get_cart", json!({}))).await;
    assert_ne!(cart["result"]["isError"], true);
    let invalid = request(
        &app,
        tool("add_to_cart", json!({"ean": BANANA, "quantity": 0})),
    )
    .await;
    assert_eq!(invalid["result"]["isError"], true);
    assert_eq!(api.events(), ["ADD-ITEM"]);
}

#[tokio::test]
async fn http_preserves_the_interactive_login_flow_across_requests() {
    let login = support::MockLogin::new().reporting("waiting", None);
    let app = router(
        CartServer::with_login(Arc::new(MockApi::new()), Arc::new(login.clone())),
        HttpConfig::default(),
        CancellationToken::new(),
    );
    let started = request(&app, tool("start_login", json!({}))).await;
    let progress: Value =
        serde_json::from_str(started["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(progress["state"], "waiting");
    assert!(
        progress["instructions"]
            .as_str()
            .unwrap()
            .contains("Switch to it")
    );
    let status = request(&app, tool("login_status", json!({}))).await;
    let progress: Value =
        serde_json::from_str(status["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(progress["state"], "waiting");
    request(&app, tool("cancel_login", json!({}))).await;
    assert_eq!(login.calls(), ["start:9222", "status", "cancel"]);
}

#[tokio::test]
async fn health_is_available_without_mcp_headers_or_browser_activity() {
    let api = MockApi::new();
    let response = app(&api, HttpConfig::default())
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn rejects_untrusted_hosts_and_origins_before_dispatch() {
    let api = MockApi::new();
    let app = app(&api, HttpConfig::default());
    for (header, value) in [("host", "evil.example"), ("origin", "https://evil.example")] {
        let mut request = rpc(tool(
            "add_to_cart",
            json!({"store_id": STORE, "ean": BANANA}),
        ));
        request.headers_mut().insert(header, value.parse().unwrap());
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{header}");
    }
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn explicit_host_and_origin_allowlists_work_without_wildcard_origins() {
    let api = MockApi::new();
    let app = app(
        &api,
        HttpConfig {
            allowed_hosts: vec!["cart.example:8080".into()],
            allowed_origins: vec!["https://chat.example".into()],
        },
    );
    for (origin, duplicate, status) in [
        ("https://chat.example", false, StatusCode::OK),
        ("https://chat.example.evil", false, StatusCode::FORBIDDEN),
        ("null", false, StatusCode::FORBIDDEN),
        ("https://chat.example", true, StatusCode::FORBIDDEN),
    ] {
        let mut request = rpc(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}));
        request
            .headers_mut()
            .insert("host", "cart.example:8080".parse().unwrap());
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
        if duplicate {
            request
                .headers_mut()
                .append("origin", origin.parse().unwrap());
        }
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), status, "{origin}, duplicate={duplicate}");
    }
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn stateless_mode_does_not_offer_a_standalone_sse_stream() {
    let app = app(&MockApi::new(), HttpConfig::default());
    for method in ["GET", "DELETE"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/mcp")
                    .header("host", "localhost")
                    .header("accept", "text/event-stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}

#[tokio::test]
async fn sdk_client_can_disconnect_and_reconnect_without_losing_shared_state() {
    let api = MockApi::new();
    let cancellation = CancellationToken::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let app = router(
        CartServer::new(Arc::new(api.clone())),
        HttpConfig::default(),
        cancellation.child_token(),
    );
    let server = tokio::spawn({
        let shutdown = cancellation.clone();
        async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .unwrap();
        }
    });
    let client = ().serve(StreamableHttpClientTransport::from_uri(url.clone())).await.unwrap();
    call_tool(&client, "set_default_store", json!({"store_id": STORE}))
        .await
        .unwrap();
    client.cancel().await.unwrap();
    let client = ().serve(StreamableHttpClientTransport::from_uri(url)).await.unwrap();
    call_tool(&client, "add_to_cart", json!({"ean": BANANA}))
        .await
        .unwrap();
    assert_eq!(api.events(), ["ADD-ITEM"]);
    client.cancel().await.unwrap();
    cancellation.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}
