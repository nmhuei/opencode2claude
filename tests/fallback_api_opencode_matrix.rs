//! Verification test suite for bidirectional model fallback:
//! - Custom/API model -> OpenCode Zen model
//! - OpenCode Zen model -> Custom/API model
//! - Multi-hop fallback chains across error types (429, 500, 503) and streaming modes

use axum::body::{to_bytes, Body, Bytes};
use axum::extract::State;
use axum::http::{header, Request, Response, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use futures_util::stream;
use opencode2api::config::{BridgeConfig, EgressMode};
use opencode2api::server::build_router;
use opencode2api::state::AppState;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tower::ServiceExt;

#[derive(Clone)]
struct FixtureState {
    fixtures: Arc<Mutex<VecDeque<Fixture>>>,
    requests: Arc<Mutex<Vec<Value>>>,
    auth_headers: Arc<Mutex<Vec<Option<String>>>>,
}

enum Fixture {
    Json(Value),
    Sse(Vec<Vec<u8>>),
    Raw {
        status: StatusCode,
        content_type: &'static str,
        chunks: Vec<Vec<u8>>,
    },
}

async fn upstream(
    State(state): State<FixtureState>,
    headers: axum::http::HeaderMap,
    Json(request): Json<Value>,
) -> Response<Body> {
    state.requests.lock().await.push(request);
    let auth = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    state.auth_headers.lock().await.push(auth);

    match state.fixtures.lock().await.pop_front().expect("fixture") {
        Fixture::Json(value) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&value).unwrap()))
            .unwrap(),
        Fixture::Sse(chunks) => {
            let body = Body::from_stream(stream::iter(
                chunks
                    .into_iter()
                    .map(|chunk| Ok::<Bytes, Infallible>(Bytes::from(chunk))),
            ));
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(body)
                .unwrap()
        }
        Fixture::Raw {
            status,
            content_type,
            chunks,
        } => {
            let body = Body::from_stream(stream::iter(
                chunks
                    .into_iter()
                    .map(|chunk| Ok::<Bytes, Infallible>(Bytes::from(chunk))),
            ));
            Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, content_type)
                .body(body)
                .unwrap()
        }
    }
}

async fn setup_harness<F>(
    fixtures: Vec<Fixture>,
    configure: F,
) -> (
    Router,
    FixtureState,
    Arc<opencode2api::observability::Metrics>,
)
where
    F: FnOnce(&mut BridgeConfig),
{
    let state = FixtureState {
        fixtures: Arc::new(Mutex::new(fixtures.into())),
        requests: Arc::new(Mutex::new(Vec::new())),
        auth_headers: Arc::new(Mutex::new(Vec::new())),
    };
    let upstream_app = Router::new()
        .route("/chat/completions", post(upstream))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let defaults = BridgeConfig::default();
    let mut config = BridgeConfig {
        model: Some("deepseek-v4-flash".to_string()),
        primary_proxies: None,
        warm_standby_proxies: None,
        retry: opencode2api::config::RetryConfig {
            upstream_base_url: format!("http://{address}"),
            max_network_attempts: 1,
            base_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
            ..defaults.retry
        },
        egress: opencode2api::config::EgressConfig {
            mode: EgressMode::Direct,
            ..defaults.egress
        },
        ..defaults
    };
    configure(&mut config);
    let app_state = AppState::new(config);
    let metrics = app_state.metrics.clone();
    (build_router(app_state), state, metrics)
}

async fn post_anthropic_messages(app: Router, model: &str, stream: bool) -> (StatusCode, String) {
    let payload = json!({
        "model": model,
        "messages": [{"role": "user", "content": "Test prompt"}],
        "stream": stream,
        "max_tokens": 128
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = String::from_utf8(
        to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    (status, body)
}

async fn post_openai_completions(app: Router, model: &str, stream: bool) -> (StatusCode, String) {
    let payload = json!({
        "model": model,
        "messages": [{"role": "user", "content": "Test prompt"}],
        "stream": stream,
        "max_tokens": 128
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = String::from_utf8(
        to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    (status, body)
}

#[tokio::test]
async fn successful_requests_rotate_configured_upstream_api_keys() {
    let success = json!({
        "id": "chatcmpl-key-rotation",
        "model": "deepseek-v4-flash",
        "choices": [{
            "message": {"content": "ok", "reasoning_content": null, "tool_calls": null},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1}
    });
    let (app, state, _) = setup_harness(
        vec![Fixture::Json(success.clone()), Fixture::Json(success)],
        |config| {
            config.retry.upstream_api_keys = vec![
                opencode2api::config::SecretString::new("key-one").expect("non-empty test key"),
                opencode2api::config::SecretString::new("key-two").expect("non-empty test key"),
            ];
        },
    )
    .await;

    let (first_status, _) = post_openai_completions(app.clone(), "deepseek-v4-flash", false).await;
    let (second_status, _) = post_openai_completions(app, "deepseek-v4-flash", false).await;

    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(second_status, StatusCode::OK);
    assert_eq!(
        *state.auth_headers.lock().await,
        vec![
            Some("Bearer key-one".to_string()),
            Some("Bearer key-two".to_string()),
        ]
    );
}

#[tokio::test]
async fn rate_limited_request_retries_with_the_next_configured_upstream_api_key() {
    let success = json!({
        "id": "chatcmpl-key-failover",
        "model": "deepseek-v4-flash",
        "choices": [{
            "message": {"content": "ok", "reasoning_content": null, "tool_calls": null},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1}
    });
    let (app, state, _) = setup_harness(
        vec![
            Fixture::Raw {
                status: StatusCode::TOO_MANY_REQUESTS,
                content_type: "text/plain",
                chunks: vec![b"rate limited".to_vec()],
            },
            Fixture::Json(success),
        ],
        |config| {
            config.retry.upstream_api_keys = vec![
                opencode2api::config::SecretString::new("key-one").expect("non-empty test key"),
                opencode2api::config::SecretString::new("key-two").expect("non-empty test key"),
            ];
        },
    )
    .await;

    let (status, _) = post_openai_completions(app, "deepseek-v4-flash", false).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        *state.auth_headers.lock().await,
        vec![
            Some("Bearer key-one".to_string()),
            Some("Bearer key-two".to_string()),
        ]
    );
}

#[tokio::test]
async fn body_encoded_rate_limit_retries_with_the_next_configured_upstream_api_key() {
    let success = json!({
        "id": "chatcmpl-key-400-failover",
        "model": "deepseek-v4-flash",
        "choices": [{
            "message": {"content": "ok", "reasoning_content": null, "tool_calls": null},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1}
    });
    let (app, state, _) = setup_harness(
        vec![
            Fixture::Raw {
                status: StatusCode::BAD_REQUEST,
                content_type: "text/plain",
                chunks: vec![b"rate limit exceeded".to_vec()],
            },
            Fixture::Json(success),
        ],
        |config| {
            config.retry.upstream_api_keys = vec![
                opencode2api::config::SecretString::new("key-one").expect("non-empty test key"),
                opencode2api::config::SecretString::new("key-two").expect("non-empty test key"),
            ];
        },
    )
    .await;

    let (status, _) = post_openai_completions(app, "deepseek-v4-flash", false).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        *state.auth_headers.lock().await,
        vec![
            Some("Bearer key-one".to_string()),
            Some("Bearer key-two".to_string()),
        ]
    );
}

#[tokio::test]
async fn rate_limit_retry_honors_configured_backoff() {
    let success = json!({
        "id": "chatcmpl-backoff",
        "model": "deepseek-v4-flash",
        "choices": [{
            "message": {"content": "ok", "reasoning_content": null, "tool_calls": null},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1}
    });
    let (app, _, _) = setup_harness(
        vec![
            Fixture::Raw {
                status: StatusCode::TOO_MANY_REQUESTS,
                content_type: "text/plain",
                chunks: vec![b"rate limited".to_vec()],
            },
            Fixture::Json(success),
        ],
        |config| {
            config.retry.base_backoff = Duration::from_secs(5);
            config.retry.max_backoff = Duration::from_secs(5);
        },
    )
    .await;

    let started = Instant::now();
    let (status, _) = post_openai_completions(app, "deepseek-v4-flash", false).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        started.elapsed() >= Duration::from_millis(4_800),
        "rate-limit retry ignored configured backoff: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn test_fallback_from_api_to_opencode_on_rate_limit_429() {
    let success = json!({
        "id": "chatcmpl-opencode-ok",
        "model": "mimo-v2.5-free",
        "choices": [{
            "message": {"content": "Response from OpenCode Zen", "reasoning_content": null, "tool_calls": null},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 8}
    });

    let fixtures = vec![
        Fixture::Raw {
            status: StatusCode::TOO_MANY_REQUESTS,
            content_type: "text/plain",
            chunks: vec![b"API Rate limit exceeded".to_vec()],
        },
        Fixture::Json(success),
    ];

    let (app, state, metrics) = setup_harness(fixtures, |config| {
        config.model = Some("deepseek-v4-flash".to_string());
        config.retry.model_fallbacks = vec!["opencode/mimo-v2.5-free".to_string()];
    })
    .await;

    let (status, body) = post_anthropic_messages(app, "claude-opus-5", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Response from OpenCode Zen"));

    let requests = state.requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["model"], "deepseek-v4-flash");
    assert_eq!(requests[1]["model"], "opencode/mimo-v2.5-free");

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.model_fallbacks, 1);
}

#[tokio::test]
async fn test_fallback_from_api_to_opencode_on_server_error_503() {
    let success = json!({
        "id": "chatcmpl-opencode-nemotron",
        "model": "nemotron-3-ultra-free",
        "choices": [{
            "message": {"content": "Response from Nemotron OpenCode", "reasoning_content": null, "tool_calls": null},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 12, "completion_tokens": 6}
    });

    let fixtures = vec![
        Fixture::Raw {
            status: StatusCode::SERVICE_UNAVAILABLE,
            content_type: "text/plain",
            chunks: vec![b"Upstream 503 Service Unavailable".to_vec()],
        },
        Fixture::Json(success),
    ];

    let (app, state, metrics) = setup_harness(fixtures, |config| {
        config.model = Some("glm-5.3-flash".to_string());
        config.retry.model_fallbacks = vec!["opencode/nemotron-3-ultra-free".to_string()];
    })
    .await;

    let (status, body) = post_openai_completions(app, "glm-5.3-flash", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Response from Nemotron OpenCode"));

    let requests = state.requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["model"], "glm-5.3-flash");
    assert_eq!(requests[1]["model"], "opencode/nemotron-3-ultra-free");

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.model_fallbacks, 1);
}

#[tokio::test]
async fn test_fallback_from_opencode_to_api_on_rate_limit_429() {
    let success = json!({
        "id": "chatcmpl-api-deepseek",
        "model": "deepseek-v4-flash",
        "choices": [{
            "message": {"content": "Response from API DeepSeek", "reasoning_content": null, "tool_calls": null},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 15, "completion_tokens": 10}
    });

    let fixtures = vec![
        Fixture::Raw {
            status: StatusCode::TOO_MANY_REQUESTS,
            content_type: "text/plain",
            chunks: vec![b"OpenCode Zen quota reached".to_vec()],
        },
        Fixture::Json(success),
    ];

    let (app, state, metrics) = setup_harness(fixtures, |config| {
        config.model = Some("opencode/mimo-v2.5-free".to_string());
        config.retry.model_fallbacks = vec!["deepseek-v4-flash".to_string()];
    })
    .await;

    let (status, body) = post_anthropic_messages(app, "claude-opus-5", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Response from API DeepSeek"));

    let requests = state.requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["model"], "opencode/mimo-v2.5-free");
    assert_eq!(requests[1]["model"], "deepseek-v4-flash");

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.model_fallbacks, 1);
}

#[tokio::test]
async fn test_fallback_from_opencode_to_api_on_server_error_500() {
    let success = json!({
        "id": "chatcmpl-api-qwen38",
        "model": "qwen3.8-flash",
        "choices": [{
            "message": {"content": "Response from Qwen 3.8 API", "reasoning_content": null, "tool_calls": null},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 20, "completion_tokens": 15}
    });

    let fixtures = vec![
        Fixture::Raw {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            content_type: "text/plain",
            chunks: vec![b"OpenCode internal error".to_vec()],
        },
        Fixture::Json(success),
    ];

    let (app, state, metrics) = setup_harness(fixtures, |config| {
        config.model = Some("opencode/deepseek-v4-flash-free".to_string());
        config.retry.model_fallbacks = vec!["qwen3.8-flash".to_string()];
    })
    .await;

    let (status, body) =
        post_openai_completions(app, "opencode/deepseek-v4-flash-free", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Response from Qwen 3.8 API"));

    let requests = state.requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["model"], "opencode/deepseek-v4-flash-free");
    assert_eq!(requests[1]["model"], "qwen3.8-flash");

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.model_fallbacks, 1);
}

#[tokio::test]
async fn test_multi_hop_fallback_api_to_opencode_to_api() {
    let final_success = json!({
        "id": "chatcmpl-final-qwen",
        "model": "qwen3.8-flash",
        "choices": [{
            "message": {"content": "Final hop succeeded on Qwen 3.8", "reasoning_content": null, "tool_calls": null},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 25, "completion_tokens": 12}
    });

    let fixtures = vec![
        // Hop 1: API model rate limited
        Fixture::Raw {
            status: StatusCode::TOO_MANY_REQUESTS,
            content_type: "text/plain",
            chunks: vec![b"API primary 429".to_vec()],
        },
        // Hop 2: OpenCode free model server error
        Fixture::Raw {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            content_type: "text/plain",
            chunks: vec![b"OpenCode 500 error".to_vec()],
        },
        // Hop 3: Secondary API model succeeds
        Fixture::Json(final_success),
    ];

    let (app, state, metrics) = setup_harness(fixtures, |config| {
        config.model = Some("deepseek-v4-flash".to_string());
        config.retry.model_fallbacks = vec![
            "opencode/mimo-v2.5-free".to_string(),
            "qwen3.8-flash".to_string(),
        ];
    })
    .await;

    let (status, body) = post_anthropic_messages(app, "claude-opus-5", false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Final hop succeeded on Qwen 3.8"));

    let requests = state.requests.lock().await;
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0]["model"], "deepseek-v4-flash");
    assert_eq!(requests[1]["model"], "opencode/mimo-v2.5-free");
    assert_eq!(requests[2]["model"], "qwen3.8-flash");

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.model_fallbacks, 2);
}

#[tokio::test]
async fn test_streaming_fallback_from_api_to_opencode() {
    let sse_chunk = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({
            "choices": [{
                "delta": {"content": "Streaming response from OpenCode fallback"},
                "finish_reason": null
            }]
        })
    );

    let fixtures = vec![
        Fixture::Raw {
            status: StatusCode::TOO_MANY_REQUESTS,
            content_type: "text/plain",
            chunks: vec![b"Stream API rate limit".to_vec()],
        },
        Fixture::Sse(vec![sse_chunk.into_bytes()]),
    ];

    let (app, state, metrics) = setup_harness(fixtures, |config| {
        config.model = Some("deepseek-v4-flash".to_string());
        config.retry.model_fallbacks = vec!["opencode/mimo-v2.5-free".to_string()];
    })
    .await;

    let (status, body) = post_anthropic_messages(app, "claude-opus-5", true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Streaming response from OpenCode fallback"));

    let requests = state.requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["model"], "deepseek-v4-flash");
    assert_eq!(requests[1]["model"], "opencode/mimo-v2.5-free");

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.model_fallbacks, 1);
}

#[tokio::test]
async fn test_streaming_fallback_from_opencode_to_api() {
    let sse_chunk = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({
            "choices": [{
                "delta": {"content": "Streaming response from API fallback"},
                "finish_reason": null
            }]
        })
    );

    let fixtures = vec![
        Fixture::Raw {
            status: StatusCode::SERVICE_UNAVAILABLE,
            content_type: "text/plain",
            chunks: vec![b"Stream OpenCode 503".to_vec()],
        },
        Fixture::Sse(vec![sse_chunk.into_bytes()]),
    ];

    let (app, state, metrics) = setup_harness(fixtures, |config| {
        config.model = Some("opencode/mimo-v2.5-free".to_string());
        config.retry.model_fallbacks = vec!["glm-5.3-flash".to_string()];
    })
    .await;

    let (status, body) = post_openai_completions(app, "opencode/mimo-v2.5-free", true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Streaming response from API fallback"));

    let requests = state.requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["model"], "opencode/mimo-v2.5-free");
    assert_eq!(requests[1]["model"], "glm-5.3-flash");

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.model_fallbacks, 1);
}
