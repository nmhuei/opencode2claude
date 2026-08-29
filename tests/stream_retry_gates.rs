//! Regression: stream retry gates must be per-attempt, not turn-global.
//!
//! The block tracker deliberately never resets (monotonic block indices within
//! one Anthropic message, see execute.rs), so `has_any_blocks_ever_opened()`
//! is sticky across search/interception rounds. Two retry paths gated on that
//! sticky flag regressed:
//!
//! 1. Compat-tool retry: a truncated `[Requesting Tool execution: ...]` marker
//!    in the round AFTER a search interception must still retry (the round
//!    itself emitted nothing), but the sticky flag dead-ends it into a terminal
//!    "repeatedly emitted an incomplete tool request" text.
//! 2. Stream-read retry: an oversized SSE line already reported to the client
//!    as an `error` event must not replay the upstream request (the client saw
//!    a terminal-ish event; a replay emits orphaned content after it).
//!
//! Both gates must therefore compare per-attempt block allocation, and the
//! line-limit failure must not be retryable at all.

use axum::body::{to_bytes, Body, Bytes};
use axum::extract::State;
use axum::http::{header, Request, Response, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream;
use opencode2api::config::{BridgeConfig, EgressConfig, EgressMode};
use opencode2api::server::build_router;
use opencode2api::state::AppState;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tower::ServiceExt;

#[derive(Clone)]
struct FixtureState {
    fixtures: Arc<Mutex<VecDeque<Fixture>>>,
    requests: Arc<Mutex<Vec<Value>>>,
}

enum Fixture {
    Sse(Vec<Vec<u8>>),
}

async fn upstream(State(state): State<FixtureState>, Json(request): Json<Value>) -> Response<Body> {
    state.requests.lock().await.push(request);
    let Fixture::Sse(chunks) = state.fixtures.lock().await.pop_front().expect("fixture");
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream::iter(
            chunks
                .into_iter()
                .map(|chunk| Ok::<Bytes, Infallible>(Bytes::from(chunk))),
        )))
        .unwrap()
}

fn sse_delta(payload: Value) -> String {
    format!(
        "data: {}\n\n",
        json!({"choices": [{"delta": payload, "finish_reason": null}]})
    )
}

fn sse_done() -> String {
    "data: [DONE]\n\n".to_string()
}

async fn harness(
    fixtures: Vec<Fixture>,
    max_sse_line_bytes: usize,
    searxng_url: Option<String>,
) -> (Router, FixtureState) {
    let state = FixtureState {
        fixtures: Arc::new(Mutex::new(fixtures.into())),
        requests: Arc::new(Mutex::new(Vec::new())),
    };
    let upstream_app = Router::new()
        .route("/chat/completions", post(upstream))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, upstream_app).await.unwrap();
    });

    let searxng_base = match searxng_url {
        Some(suffix) => {
            let searxng_app = Router::new().route(
                "/search",
                get(|| async {
                    Json(json!({
                        "results": [
                            {"title": "Local result", "url": "https://example.com/local", "content": "snippet"}
                        ]
                    }))
                }),
            );
            let searxng_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let searxng_address = searxng_listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(searxng_listener, searxng_app).await.unwrap();
            });
            Some(format!("http://{searxng_address}{suffix}"))
        }
        None => None,
    };

    let defaults = BridgeConfig::default();
    let config = BridgeConfig {
        model: Some("fixture-model".to_string()),
        primary_proxies: None,
        warm_standby_proxies: None,
        retry: opencode2api::config::RetryConfig {
            upstream_base_url: format!("http://{address}"),
            max_network_attempts: 1,
            ..defaults.retry
        },
        egress: EgressConfig {
            mode: EgressMode::Direct,
            ..defaults.egress
        },
        protocol: opencode2api::config::ProtocolConfig {
            max_sse_line_bytes,
            ..defaults.protocol
        },
        searxng_url: searxng_base,
        ..defaults
    };
    (build_router(AppState::new(config)), state)
}

fn anthropic_request(stream: bool) -> Value {
    json!({
        "model": "fixture-model",
        "messages": [{"role": "user", "content": "Hello fixture"}],
        "tools": [
            {"name": "web_search", "description": "search the web", "input_schema": {"type": "object"}},
            {"name": "Bash", "description": "execute a shell command", "input_schema": {"type": "object"}}
        ],
        "stream": stream,
        "max_tokens": 128
    })
}

async fn call(app: Router, body: Value) -> (StatusCode, String) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
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
async fn oversized_line_reports_error_without_replaying_upstream() {
    // BUG-007: the line-limit failure emits an SSE error event to the client,
    // so replaying the request (current behavior: retry gate passes when no
    // block was opened) would emit orphaned content after a terminal error
    // event, and burn an extra upstream request.
    let huge = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}},\"finish_reason\":null}}]}}\n\n",
        "x".repeat(4096)
    );
    let (app, state) = harness(vec![Fixture::Sse(vec![huge.into_bytes()])], 1024, None).await;
    let (status, body) = call(app, anthropic_request(true)).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("event: error"), "{body}");
    assert!(body.contains("exceeded configured byte limit"), "{body}");
    assert_eq!(body.matches("event: message_start").count(), 1, "{body}");
    // The error event is terminal: message_stop never follows it.
    assert_eq!(body.matches("event: message_stop").count(), 0, "{body}");
    assert_eq!(
        state.requests.lock().await.len(),
        1,
        "line-limit failure must not replay the upstream request"
    );
}

#[tokio::test]
async fn compat_retry_fires_after_search_round() {
    // BUG-003: round 1 streams text BEFORE the search marker, so the text
    // fragment is emitted and a content block opens (the interception guard
    // is only set when the marker itself is parsed). A subsequent round that
    // emits only a truncated compat marker must still retry — the round
    // itself emitted nothing, so replay is safe and must not be blocked by
    // the turn-global sticky block tracker.
    let search_chunk = sse_delta(json!({
        "content": "Researching: ",
        "tool_calls": [{
            "index": 0,
            "id": "tc_search_1",
            "type": "function",
            "function": {
                "name": "web_search",
                "arguments": "{\"query\":\"rust async\"}"
            }
        }]
    }));
    let truncated_marker = sse_delta(
        json!({"content": "[Requesting Tool execution: 'Bash' with arguments:{\"command\":\"echo unfinished\""}),
    );
    let bash_marker = sse_delta(json!({
        "content": "[Requesting Tool execution: 'Bash' with arguments:{\"command\":\"echo ok\"}]"
    }));
    let (app, state) = harness(
        vec![
            Fixture::Sse(vec![search_chunk.into_bytes(), sse_done().into_bytes()]),
            Fixture::Sse(vec![truncated_marker.into_bytes(), sse_done().into_bytes()]),
            Fixture::Sse(vec![bash_marker.into_bytes(), sse_done().into_bytes()]),
        ],
        256 * 1024,
        Some("/".to_string()),
    )
    .await;

    let (status, body) = call(app, anthropic_request(true)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        state.requests.lock().await.len(),
        3,
        "compat retry must fire in the round after a search interception"
    );
    assert!(
        !body.contains("repeatedly emitted an incomplete tool request"),
        "{body}"
    );
    assert!(body.contains("\"name\":\"Bash\""), "{body}");
    assert!(body.contains("\"type\":\"tool_use\""), "{body}");
}
