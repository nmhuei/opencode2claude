//! Tests for Super-Router multi-account rotation, per-model lock isolation, and combo fallback.

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use opencode2api::config::{BridgeConfig, EgressMode};
use opencode2api::router::accounts::Account;
use opencode2api::router::combos::{ComboStrategy, ModelCombo};
use opencode2api::server::build_router;
use opencode2api::state::AppState;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tower::ServiceExt;

#[derive(Clone)]
struct MockUpstreamState {
    acc1_requests: Arc<AtomicUsize>,
    acc2_requests: Arc<AtomicUsize>,
}

async fn mock_chat_completions(
    State(mock): State<MockUpstreamState>,
    headers: axum::http::HeaderMap,
    Json(payload): Json<Value>,
) -> axum::response::Response {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();

    let model = payload.get("model").and_then(|m| m.as_str()).unwrap_or_default();

    if model == "fail-model" {
        return axum::response::Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::from(r#"{"error":{"message":"service unavailable"}}"#))
            .unwrap();
    }

    if auth.contains("key-1") {
        mock.acc1_requests.fetch_add(1, Ordering::SeqCst);
        if model == "rate-limited-model" {
            // Return 429 Rate Limit for Account 1 on this specific model
            return axum::response::Response::builder()
                .status(StatusCode::TOO_MANY_REQUESTS)
                .header(header::CONTENT_TYPE, "application/json")
                .header("retry-after", "10")
                .body(Body::from(
                    r#"{"error":{"message":"Rate limit exceeded for acc 1","type":"rate_limit_error"}}"#,
                ))
                .unwrap();
        }
        // Other models succeed on Account 1
        return axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"id":"chatcmpl-acc1","object":"chat.completion","created":1234,"model":"other-model","choices":[{"index":0,"message":{"role":"assistant","content":"success from acc 1"},"finish_reason":"stop"}]}"#,
            ))
            .unwrap();
    }

    if auth.contains("key-2") {
        mock.acc2_requests.fetch_add(1, Ordering::SeqCst);
        return axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                r#"{"id":"chatcmpl-acc2","object":"chat.completion","created":1234,"model":"rate-limited-model","choices":[{"index":0,"message":{"role":"assistant","content":"success from acc 2"},"finish_reason":"stop"}]}"#,
            ))
            .unwrap();
    }

    // Default response for working-model in combo test
    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"id":"chatcmpl-combo","object":"chat.completion","created":1234,"model":"working-model","choices":[{"index":0,"message":{"role":"assistant","content":"success from combo fallback"},"finish_reason":"stop"}]}"#,
        ))
        .unwrap()
}

#[tokio::test]
async fn test_multi_account_429_failover_and_per_model_lock_isolation() {
    let mock = MockUpstreamState {
        acc1_requests: Arc::new(AtomicUsize::new(0)),
        acc2_requests: Arc::new(AtomicUsize::new(0)),
    };

    let upstream_app = Router::new()
        .route("/chat/completions", post(mock_chat_completions))
        .with_state(mock.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let defaults = BridgeConfig::default();
    let config = BridgeConfig {
        model: None,
        retry: opencode2api::config::RetryConfig {
            upstream_base_url: format!("http://{address}"),
            max_network_attempts: 2,
            base_backoff: Duration::ZERO,
            max_backoff: Duration::from_millis(50),
            ..defaults.retry
        },
        egress: opencode2api::config::EgressConfig {
            mode: EgressMode::Direct,
            ..defaults.egress
        },
        ..defaults
    };

    let state = AppState::new(config);

    // Register two accounts in pool: acc-1 (priority 1) and acc-2 (priority 2)
    {
        let mut pool = state.account_pool.write().await;
        pool.add_account(Account {
            id: "cline-acc-1".to_string(),
            name: "Account 1".to_string(),
            provider: "default".to_string(),
            api_key: Some("key-1".to_string()),
            access_token: None,
            refresh_token: None,
            email: None,
            expires_at: None,
            priority: 1,
            is_active: true,
        });
        pool.add_account(Account {
            id: "cline-acc-2".to_string(),
            name: "Account 2".to_string(),
            provider: "default".to_string(),
            api_key: Some("key-2".to_string()),
            access_token: None,
            refresh_token: None,
            email: None,
            expires_at: None,
            priority: 2,
            is_active: true,
        });
    }

    let app = build_router(state.clone());

    // Request 1: targets "rate-limited-model".
    // Account 1 should try first, hit 429, lock (acc-1, rate-limited-model), and failover to Account 2!
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "rate-limited-model",
                        "messages": [{"role": "user", "content": "hello"}],
                        "max_tokens": 100
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(mock.acc1_requests.load(Ordering::SeqCst), 1);
    assert_eq!(mock.acc2_requests.load(Ordering::SeqCst), 1);

    // Verify Account 1 is now locked for "rate-limited-model"
    {
        let pool = state.account_pool.read().await;
        assert!(pool.lock_tracker().is_model_locked("cline-acc-1", "rate-limited-model"));
        // But Account 1 is NOT locked for "other-model" (Per-Model Lock Isolation!)
        assert!(!pool.lock_tracker().is_model_locked("cline-acc-1", "other-model"));
    }

    // Request 2: targets "other-model".
    // Since Account 1 has higher priority (1 vs 2) and is NOT locked for "other-model",
    // Account 1 should be selected and succeed without hitting Account 2!
    let response2 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "other-model",
                        "messages": [{"role": "user", "content": "hello"}],
                        "max_tokens": 100
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response2.status(), StatusCode::OK);
    assert_eq!(mock.acc1_requests.load(Ordering::SeqCst), 2);
    // Account 2 should NOT have been touched for the second request
    assert_eq!(mock.acc2_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_combo_fallback_execution() {
    let mock = MockUpstreamState {
        acc1_requests: Arc::new(AtomicUsize::new(0)),
        acc2_requests: Arc::new(AtomicUsize::new(0)),
    };

    let upstream_app = Router::new()
        .route("/chat/completions", post(mock_chat_completions))
        .with_state(mock.clone());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let defaults = BridgeConfig::default();
    let config = BridgeConfig {
        model: Some("coder-combo".to_string()),
        retry: opencode2api::config::RetryConfig {
            upstream_base_url: format!("http://{address}"),
            max_network_attempts: 1,
            base_backoff: Duration::ZERO,
            max_backoff: Duration::from_millis(50),
            ..defaults.retry
        },
        egress: opencode2api::config::EgressConfig {
            mode: EgressMode::Direct,
            ..defaults.egress
        },
        ..defaults
    };

    let state = AppState::new(config);

    // Register combo "coder-combo" = ["fail-model", "working-model"]
    state.combo_resolver.register_combo(ModelCombo {
        name: "coder-combo".to_string(),
        models: vec!["fail-model".to_string(), "working-model".to_string()],
        strategy: ComboStrategy::Fallback,
    });

    let app = build_router(state.clone());

    // Request targets "coder-combo".
    // First candidate "fail-model" returns 502, triggering model fallback to "working-model"!
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({
                        "model": "coder-combo",
                        "messages": [{"role": "user", "content": "hello"}],
                        "max_tokens": 100
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}
