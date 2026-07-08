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
    let config = opencode2api::config::BridgeConfig::default();
    let state = opencode2api::state::AppState::new(config);

    Router::new()
        .route(
            "/v1/messages",
            post(opencode2api::handlers::handle_messages),
        )
        .route("/v1/models", get(opencode2api::handlers::handle_models))
        .route("/health", get(opencode2api::handlers::handle_health))
        .route("/", get(opencode2api::dashboard::serve_landing))
        .route("/dashboard", get(opencode2api::dashboard::serve_webui))
        .route("/dashboard/", get(opencode2api::dashboard::serve_webui))
        .route(
            "/dashboard/*path",
            get(opencode2api::dashboard::serve_webui),
        )
        .route(
            "/api/dashboard/status",
            get(opencode2api::dashboard::handler_rest_status),
        )
        .route(
            "/api/dashboard/diagnostics",
            get(opencode2api::dashboard::handler_dashboard_diagnostics),
        )
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
        body["status"], "ok",
        "Health body should report minimal status ok"
    );
    assert!(
        body["daemon"].is_null(),
        "daemon metadata should be stripped from anonymous health check"
    );
    assert!(
        body["config"].is_null(),
        "config metadata should be stripped from anonymous health check"
    );
    assert!(
        body["proxy_pool"].is_null(),
        "proxy_pool metadata should be stripped from anonymous health check"
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
        403,
        "Default shell policy is Disabled — shell commands should be rejected"
    );
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

static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[tokio::test]
async fn test_dashboard_auth_fast() {
    let _lock = ENV_MUTEX.lock().unwrap();

    // 1. When DASHBOARD_ADMIN_TOKEN is unset/empty
    std::env::remove_var("DASHBOARD_ADMIN_TOKEN");
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    // Request with 123456 token should be rejected (401)
    let resp1 = client
        .get(format!("{}/api/dashboard/status", base))
        .header("X-Dashboard-Token", "123456")
        .send()
        .await
        .unwrap();
    assert_eq!(resp1.status(), 401);

    // Request without token should be rejected (401)
    let resp2 = client
        .get(format!("{}/api/dashboard/status", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), 401);

    // 2. When DASHBOARD_ADMIN_TOKEN is set explicitly
    std::env::set_var("DASHBOARD_ADMIN_TOKEN", "super-secret-admin-token-12345");
    let base2 = spawn_test_server().await;

    // Request with correct token should be accepted (200)
    let resp3 = client
        .get(format!("{}/api/dashboard/status", base2))
        .header("X-Dashboard-Token", "super-secret-admin-token-12345")
        .send()
        .await
        .unwrap();
    assert_eq!(resp3.status(), 200);

    // Request with 123456 token should still be rejected (401)
    let resp4 = client
        .get(format!("{}/api/dashboard/status", base2))
        .header("X-Dashboard-Token", "123456")
        .send()
        .await
        .unwrap();
    assert_eq!(resp4.status(), 401);

    std::env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_authenticated_diagnostics_fast() {
    let _lock = ENV_MUTEX.lock().unwrap();
    std::env::set_var("DASHBOARD_ADMIN_TOKEN", "super-secret-admin-token-12345");
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    // Anonymous diagnostics request should be rejected (401)
    let resp1 = client
        .get(format!("{}/api/dashboard/diagnostics", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp1.status(), 401);

    // Correctly authenticated request should succeed (200) and return rich operational details
    let resp2 = client
        .get(format!("{}/api/dashboard/diagnostics", base))
        .header("X-Dashboard-Token", "super-secret-admin-token-12345")
        .send()
        .await
        .unwrap();
    assert_eq!(resp2.status(), 200);

    let body: Value = resp2.json().await.unwrap();
    assert_eq!(body["status"], "healthy");
    assert!(body["daemon"]["port"].as_u64().is_some());
    assert!(body["config"]["shell_policy"].as_str().is_some());
    assert!(body["proxy_pool"].is_object());

    std::env::remove_var("DASHBOARD_ADMIN_TOKEN");
}

#[tokio::test]
async fn test_security_headers_fast() {
    let base = spawn_test_server().await;
    let client = reqwest::Client::new();

    for path in &["", "dashboard", "dashboard/"] {
        let resp = client
            .get(format!("{}/{}", base, path))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);

        let headers = resp.headers();
        assert!(headers.contains_key("content-security-policy"));
        assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
        assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
        assert_eq!(headers.get("referrer-policy").unwrap(), "no-referrer");
    }
}
