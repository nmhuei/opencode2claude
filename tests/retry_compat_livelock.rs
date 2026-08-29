//! Regression: a persistent reasoning-content HTTP 400 must terminate with a
//! bounded number of upstream requests.
//!
//! The retry sanitizer has two strategies with inverse effects on
//! `reasoning_content` (repair inserts a placeholder, disable strips it), so a
//! provider that keeps rejecting `reasoning_content` must not oscillate
//! forever. Each sanitize round must be capped, after which the normal retry
//! budget (with backoff) takes over and the request fails or advances model.

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use opencode2api::config::{BridgeConfig, EgressMode};
use opencode2api::server::build_router;
use opencode2api::state::AppState;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tower::ServiceExt;

#[derive(Clone)]
struct RequestCounter(Arc<AtomicUsize>);

async fn compat_400_upstream(
    State(counter): State<RequestCounter>,
    Json(_request): Json<Value>,
) -> axum::response::Response {
    counter.0.fetch_add(1, Ordering::SeqCst);
    axum::response::Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"error":{"message":"reasoning_content unsupported for this model","type":"invalid_request_error"}}"#,
        ))
        .unwrap()
}

#[tokio::test]
async fn persistent_reasoning_compat_400_terminates_bounded() {
    let counter = RequestCounter(Arc::new(AtomicUsize::new(0)));
    let upstream_app = Router::new()
        .route("/chat/completions", post(compat_400_upstream))
        .with_state(counter.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let defaults = BridgeConfig::default();
    let config = BridgeConfig {
        model: Some("fixture-model".to_string()),
        retry: opencode2api::config::RetryConfig {
            upstream_base_url: format!("http://{address}"),
            max_network_attempts: 1,
            base_backoff: Duration::ZERO,
            ..defaults.retry
        },
        egress: opencode2api::config::EgressConfig {
            mode: EgressMode::Direct,
            ..defaults.egress
        },
        ..defaults
    };
    let app = build_router(AppState::new(config));

    // Assistant tool_use without reasoning triggers repair_missing_tool_reasoning;
    // the fixture upstream 400s on reasoning_content forever.
    let payload = json!({
        "model": "fixture-model",
        "messages": [
            {"role": "user", "content": "run the tool"},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "call-1", "name": "Read", "input": {"file_path": "/tmp/x"}}
            ]}
        ],
        "max_tokens": 64,
        "stream": false
    });

    let response = tokio::time::timeout(
        Duration::from_secs(10),
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        ),
    )
    .await
    .expect("retry loop must terminate; unbounded sanitization is a livelock")
    .expect("oneshot cannot fail");

    assert!(
        !response.status().is_success(),
        "fixture upstream never succeeds"
    );
    let calls = counter.0.load(Ordering::SeqCst);
    // 2 sanitize rounds + 1 budgeted retry + 1 terminal attempt; headroom for
    // future escalation changes.
    assert!(
        calls <= 6,
        "sanitize rounds must be bounded, got {calls} upstream requests"
    );
}
