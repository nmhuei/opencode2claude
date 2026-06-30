//! Fast integration smoke tests — no release build required.
//!
//! These tests run in-process, spawning the axum router on a random port.
//! They do NOT require `cargo build --release`, Docker, WARP, network,
//! upstream LLM, or OpenCode CLI.
//!
//! Run: `cargo test --test fast`

use axum::routing::{get, post};
use axum::Router;
use serde_json::Value;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::sleep;
use tower_http::limit::RequestBodyLimitLayer;

/// Build the same router structure used in production, with test config.
fn build_test_router() -> Router {
    let config = opencode2claude::config::BridgeConfig::default();
    let state = opencode2claude::state::AppState::new(config);

    Router::new()
        .route(
            "/v1/messages",
            post(opencode2claude::handlers::handle_messages),
        )
        .route("/v1/models", get(opencode2claude::handlers::handle_models))
        .route("/health", get(opencode2claude::handlers::handle_health))
        .layer(RequestBodyLimitLayer::new(1_048_576))
        .with_state(state)
}

/// Start test server on a random port, return base_url.
/// Retries health check until server is ready.
async fn spawn_test_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{}", addr);

    let app = build_test_router();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Poll /health until ready
    let client = reqwest::Client::new();
    for _ in 0..20 {
        if let Ok(resp) = client.get(format!("{}/health", base)).send().await {
            if resp.status() == 200 {
                return base;
            }
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("Server failed to start within timeout");
}

#[tokio::test]
async fn test_health_endpoint_fast() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/health", base))
        .send()
        .await
        .expect("GET /health should succeed");

    assert_eq!(resp.status(), 200, "/health should return 200");

    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["status"], "healthy",
        "Health body should report healthy"
    );
    assert!(
        body["daemon"]["port"].as_u64().is_some(),
        "daemon port should exist"
    );
    assert!(
        body["config"]["shell_policy"].as_str().is_some(),
        "config shell_policy should exist"
    );
}

#[tokio::test]
async fn test_models_endpoint_fast() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/v1/models", base))
        .send()
        .await
        .expect("GET /v1/models should succeed");

    assert_eq!(resp.status(), 200, "/v1/models should return 200");

    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["object"], "list", "models should be a list");
    assert!(body["data"].is_array(), "data should be an array");
    assert!(
        !body["data"].as_array().unwrap().is_empty(),
        "data should not be empty"
    );
    assert!(
        body["data"][0]["id"].as_str().is_some(),
        "each model should have an id"
    );
}

#[tokio::test]
async fn test_shell_disabled_default_fast() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    let body = serde_json::json!({
        "model": "test-model",
        "messages": [{"role": "user", "content": "!echo test"}],
        "stream": false
    });

    let resp = client
        .post(format!("{}/v1/messages", base))
        .json(&body)
        .send()
        .await
        .expect("POST /v1/messages should respond");

    assert_eq!(
        resp.status(),
        200,
        "Shell command delegation should return 200"
    );

    let val: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(val["content"][0]["type"], "tool_use");
    assert_eq!(val["content"][0]["input"]["command"], "echo test");
}

#[tokio::test]
async fn test_invalid_route_404_fast() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/nonexistent", base))
        .send()
        .await
        .expect("GET /nonexistent should respond");
    assert_eq!(resp.status(), 404, "Unknown route should return 404");
}

#[tokio::test]
async fn test_empty_messages_returns_error_fast() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    let body = serde_json::json!({
        "model": "test-model",
        "messages": [],
        "stream": false
    });

    let resp = client
        .post(format!("{}/v1/messages", base))
        .json(&body)
        .send()
        .await
        .expect("POST /v1/messages should respond");

    let status = resp.status();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["type"], "error",
        "Empty messages should return error, got status {}",
        status
    );
}
