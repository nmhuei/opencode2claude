//! Deterministic Anthropic/OpenAI protocol conformance through the production router.

use axum::body::{to_bytes, Body, Bytes};
use axum::extract::State;
use axum::http::{header, Request, Response, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use futures_util::{stream, StreamExt};
use opencode2api::config::{BridgeConfig, EgressMode};
use opencode2api::server::build_router;
use opencode2api::shell::ShellPolicy;
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
    Json(Value),
    Sse(Vec<Vec<u8>>),
    SseReadError(Vec<Vec<u8>>),
    Cancellable(tokio::sync::oneshot::Sender<()>),
    Raw {
        status: StatusCode,
        content_type: &'static str,
        chunks: Vec<Vec<u8>>,
    },
}

async fn upstream(State(state): State<FixtureState>, Json(request): Json<Value>) -> Response<Body> {
    state.requests.lock().await.push(request);
    match state.fixtures.lock().await.pop_front().expect("fixture") {
        Fixture::Json(value) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&value).unwrap()))
            .unwrap(),
        Fixture::Sse(chunks) => stream_response(StatusCode::OK, "text/event-stream", chunks),
        Fixture::SseReadError(chunks) => stream_error_response(chunks),
        Fixture::Cancellable(sender) => cancellable_response(sender),
        Fixture::Raw {
            status,
            content_type,
            chunks,
        } => stream_response(status, content_type, chunks),
    }
}

fn stream_error_response(chunks: Vec<Vec<u8>>) -> Response<Body> {
    let visible = stream::iter(
        chunks
            .into_iter()
            .map(|chunk| Ok::<Bytes, std::io::Error>(Bytes::from(chunk))),
    );
    let reset = stream::once(async {
        // Force an async boundary so Hyper cannot collapse the preceding data
        // chunk and terminal read error into a single failed poll. Tests that
        // distinguish pre-content from post-content failures require the
        // visible chunk to be observably delivered first.
        tokio::task::yield_now().await;
        Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "fixture upstream body reset",
        ))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(visible.chain(reset)))
        .unwrap()
}

fn stream_response(status: StatusCode, content_type: &str, chunks: Vec<Vec<u8>>) -> Response<Body> {
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

async fn harness(fixtures: Vec<Fixture>) -> (Router, FixtureState) {
    let (app, fixtures, _metrics) =
        harness_core(fixtures, 256 * 1024, 4 * 1024 * 1024, |_| {}).await;
    (app, fixtures)
}

async fn harness_with_limits(
    fixtures: Vec<Fixture>,
    max_sse_line_bytes: usize,
    max_sync_response_bytes: usize,
) -> (Router, FixtureState) {
    let (app, fixtures, _metrics) = harness_core(
        fixtures,
        max_sse_line_bytes,
        max_sync_response_bytes,
        |_| {},
    )
    .await;
    (app, fixtures)
}

async fn harness_core<F>(
    fixtures: Vec<Fixture>,
    max_sse_line_bytes: usize,
    max_sync_response_bytes: usize,
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
        model: Some("fixture-model".to_string()),
        primary_proxies: None,
        warm_standby_proxies: None,
        retry: opencode2api::config::RetryConfig {
            upstream_base_url: format!("http://{address}"),
            max_network_attempts: 1,
            max_provider_attempts: 1,
            ..defaults.retry
        },
        egress: opencode2api::config::EgressConfig {
            mode: EgressMode::Direct,
            ..defaults.egress
        },
        protocol: opencode2api::config::ProtocolConfig {
            max_sse_line_bytes,
            max_sync_response_bytes,
            ..defaults.protocol
        },
        ..defaults
    };
    configure(&mut config);
    let app_state = AppState::new(config);
    let metrics = app_state.metrics.clone();
    (build_router(app_state), state, metrics)
}

fn anthropic_request(stream: bool) -> Value {
    json!({
        "model":"fixture-model",
        "messages":[{"role":"user","content":"Hello fixture"}],
        "stream":stream,
        "max_tokens":128
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
async fn sync_reasoning_text_and_usage_are_anthropic_compatible() {
    let fixture = json!({
        "id":"chatcmpl-sync",
        "model":"upstream-model",
        "choices":[{
            "message":{
                "content":"Xin chào",
                "reasoning_content":"Suy nghĩ",
                "tool_calls":null
            },
            "finish_reason":"stop"
        }],
        "usage":{"prompt_tokens":11,"completion_tokens":7}
    });
    let (app, state) = harness(vec![Fixture::Json(fixture)]).await;
    let (status, body) = call(app, anthropic_request(false)).await;
    assert_eq!(status, StatusCode::OK);
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["type"], "message");
    assert_eq!(response["model"], "fixture-model");
    assert_eq!(response["content"][0]["type"], "thinking");
    assert_eq!(response["content"][0]["thinking"], "Suy nghĩ");
    assert_eq!(response["content"][1]["type"], "text");
    assert_eq!(response["content"][1]["text"], "Xin chào");
    assert_eq!(response["stop_reason"], "end_turn");
    assert_eq!(response["usage"]["input_tokens"], 11);
    assert_eq!(response["usage"]["output_tokens"], 7);

    let upstream_request = state.requests.lock().await[0].clone();
    assert_eq!(upstream_request["model"], "fixture-model");
    assert_eq!(upstream_request["stream"], false);
    assert_eq!(upstream_request["messages"][0]["role"], "user");
}

#[tokio::test]
async fn sync_native_and_dsml_tool_calls_map_to_tool_use() {
    let native = json!({
        "id":"chatcmpl-tool",
        "model":"upstream-model",
        "choices":[{
            "message":{
                "content":null,
                "reasoning_content":null,
                "tool_calls":[{
                    "id":"call-1",
                    "function":{"name":"Read","arguments":"{\"path\":\"README.md\"}"}
                }]
            },
            "finish_reason":"tool_calls"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1}
    });
    let dsml_text = "Before <｜DSML｜tool_calls><｜DSML｜invoke name=\"bash\"><｜DSML｜parameter name=\"command\">git status</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls> After";
    let dsml = json!({
        "id":"chatcmpl-dsml",
        "model":"upstream-model",
        "choices":[{
            "message":{"content":dsml_text,"reasoning_content":null,"tool_calls":null},
            "finish_reason":"stop"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1}
    });
    let (app, _state) = harness(vec![Fixture::Json(native), Fixture::Json(dsml)]).await;
    let mut request = anthropic_request(false);
    request["tools"] = json!([{
        "name":"Read",
        "description":"Read a file",
        "input_schema":{"type":"object","properties":{"path":{"type":"string"}}}
    },{
        "name":"bash",
        "description":"Run a shell command",
        "input_schema":{"type":"object","properties":{"command":{"type":"string"}}}
    }]);

    let (status, body) = call(app.clone(), request.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["content"][0]["type"], "tool_use");
    assert_eq!(response["content"][0]["id"], "call-1");
    assert_eq!(response["content"][0]["name"], "Read");
    assert_eq!(response["content"][0]["input"]["path"], "README.md");
    assert_eq!(response["stop_reason"], "tool_use");

    let (status, body) = call(app, request).await;
    assert_eq!(status, StatusCode::OK);
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["content"].as_array().unwrap().len(), 1);
    assert_eq!(response["content"][0]["type"], "text");
    assert_eq!(response["content"][0]["text"], "Before git status After");
    assert_eq!(response["stop_reason"], "end_turn");
}

#[tokio::test]
async fn sync_native_tool_call_preserves_visible_text_with_tool_call() {
    let fixture = json!({
        "id":"chatcmpl-sync-clipped-preamble",
        "model":"upstream-model",
        "choices":[{
            "message":{
                "content":"Proxy up (200), env đủ. Copy tinyctfer sang tools/ và đọc code container conf",
                "reasoning_content":null,
                "tool_calls":[{
                    "id":"call-sync-clipped-preamble",
                    "function":{
                        "name":"Bash",
                        "arguments":"{\"command\":\"printf PRE_TOOL_SYNC_OK\"}"
                    }
                }]
            },
            "finish_reason":"tool_calls"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1}
    });
    let (app, _state) = harness(vec![Fixture::Json(fixture)]).await;
    let mut request = anthropic_request(false);
    request["tools"] = json!([{
        "name":"Bash",
        "description":"Run a harmless command",
        "input_schema":{
            "type":"object",
            "properties":{"command":{"type":"string"}},
            "required":["command"]
        }
    }]);

    let (status, body) = call(app, request).await;
    assert_eq!(status, StatusCode::OK);
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["content"][0]["type"], "text");
    assert_eq!(
        response["content"][0]["text"],
        "Proxy up (200), env đủ. Copy tinyctfer sang tools/ và đọc code container conf"
    );
    assert_eq!(response["content"][1]["type"], "tool_use");
    assert_eq!(response["content"][1]["name"], "Bash");
    assert_eq!(
        response["content"][1]["input"]["command"],
        "printf PRE_TOOL_SYNC_OK"
    );
    assert_eq!(response["stop_reason"], "tool_use");
    assert!(body.contains("Copy tinyctfer"), "{body}");
}

#[tokio::test]
async fn sync_native_call_ignores_unavailable_encoded_marker() {
    let fixture = json!({
        "id":"chatcmpl-sync-native-wins",
        "model":"upstream-model",
        "choices":[{
            "message":{
                "content":"[Requesting MissingTool with arguments: {\"value\":\"must-not-run\"}]",
                "reasoning_content":null,
                "tool_calls":[{
                    "id":"call-sync-native-wins",
                    "function":{
                        "name":"Bash",
                        "arguments":"{\"command\":\"printf SYNC_NATIVE_WINS\"}"
                    }
                }]
            },
            "finish_reason":"tool_calls"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1}
    });
    let (app, state) = harness(vec![Fixture::Json(fixture)]).await;
    let mut request = anthropic_request(false);
    request["tools"] = json!([{
        "name":"Bash",
        "description":"Run a harmless command",
        "input_schema":{
            "type":"object",
            "properties":{"command":{"type":"string"}},
            "required":["command"]
        }
    }]);

    let (status, body) = call(app, request).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(state.requests.lock().await.len(), 1);
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["content"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!(response["content"][0]["type"], "tool_use");
    assert_eq!(response["content"][0]["id"], "call-sync-native-wins");
    assert_eq!(response["content"][0]["name"], "Bash");
    assert_eq!(
        response["content"][0]["input"]["command"],
        "printf SYNC_NATIVE_WINS"
    );
    assert!(!body.contains("MissingTool"), "{body}");
    assert!(!body.contains("must-not-run"), "{body}");
}

#[tokio::test]
async fn sync_encoded_candidate_retries_native_before_tool_use() {
    let encoded_marker = r#"[Requesting Tool execution: 'Bash' with arguments: {"command":"printf SYNC_ENCODED_SHOULD_NOT_EXECUTE"}]"#;
    let encoded = json!({
        "id":"chatcmpl-sync-encoded-first",
        "model":"upstream-model",
        "choices":[{
            "message":{"content":encoded_marker,"reasoning_content":null,"tool_calls":null},
            "finish_reason":"stop"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1}
    });
    let native = json!({
        "id":"chatcmpl-sync-native-recovery",
        "model":"upstream-model",
        "choices":[{
            "message":{
                "content":null,
                "reasoning_content":null,
                "tool_calls":[{
                    "id":"call-sync-native-recovery",
                    "function":{
                        "name":"Bash",
                        "arguments":"{\"command\":\"printf SYNC_NATIVE_RECOVERY_OK\"}"
                    }
                }]
            },
            "finish_reason":"tool_calls"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1}
    });
    let (app, state) = harness(vec![Fixture::Json(encoded), Fixture::Json(native)]).await;
    let mut request = anthropic_request(false);
    request["tools"] = json!([{
        "name":"Bash",
        "description":"Run a harmless command",
        "input_schema":{
            "type":"object",
            "properties":{"command":{"type":"string"}},
            "required":["command"]
        }
    }]);

    let (status, body) = call(app, request).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(state.requests.lock().await.len(), 2);
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["content"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!(response["content"][0]["type"], "tool_use");
    assert_eq!(response["content"][0]["id"], "call-sync-native-recovery");
    assert_eq!(response["content"][0]["name"], "Bash");
    assert_eq!(
        response["content"][0]["input"]["command"],
        "printf SYNC_NATIVE_RECOVERY_OK"
    );
    assert!(!body.contains("SYNC_ENCODED_SHOULD_NOT_EXECUTE"), "{body}");
}

#[tokio::test]
async fn sync_encoded_candidate_after_native_retry_uses_strict_fallback() {
    let first_marker = r#"[Requesting Tool execution: 'Bash' with arguments: {"command":"printf SYNC_FIRST_ENCODED_MUST_NOT_EXECUTE"}]"#;
    let fallback_marker = r#"[Requesting Tool execution: 'Bash' with arguments: {"command":"printf SYNC_STRICT_FALLBACK_OK"}]"#;
    let fixture = |id: &str, marker: &str| {
        json!({
            "id":id,
            "model":"upstream-model",
            "choices":[{
                "message":{"content":marker,"reasoning_content":null,"tool_calls":null},
                "finish_reason":"stop"
            }],
            "usage":{"prompt_tokens":1,"completion_tokens":1}
        })
    };
    let (app, state) = harness(vec![
        Fixture::Json(fixture("chatcmpl-sync-first-encoded", first_marker)),
        Fixture::Json(fixture("chatcmpl-sync-fallback-encoded", fallback_marker)),
    ])
    .await;
    let mut request = anthropic_request(false);
    request["tools"] = json!([{
        "name":"Bash",
        "description":"Run a harmless command",
        "input_schema":{
            "type":"object",
            "properties":{"command":{"type":"string"}},
            "required":["command"]
        }
    }]);

    let (status, body) = call(app, request).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(state.requests.lock().await.len(), 2);
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["content"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!(response["content"][0]["type"], "tool_use");
    assert_eq!(response["content"][0]["name"], "Bash");
    assert_eq!(
        response["content"][0]["input"]["command"],
        "printf SYNC_STRICT_FALLBACK_OK"
    );
    assert!(
        !body.contains("SYNC_FIRST_ENCODED_MUST_NOT_EXECUTE"),
        "{body}"
    );
}

#[tokio::test]
async fn fragmented_utf8_stream_preserves_reasoning_then_text_order() {
    let wire = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Suy nghĩ \"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"Xin chào Việt Nam\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    )
    .as_bytes()
    .to_vec();
    let text = std::str::from_utf8(&wire).unwrap();
    let marker = text.find("Việt").expect("UTF-8 marker");
    // V + i are two ASCII bytes; add one byte to cut inside the 3-byte `ệ`.
    let split = marker + 3;
    let chunks = vec![
        wire[..17].to_vec(),
        wire[17..split].to_vec(),
        wire[split..].to_vec(),
    ];
    let (app, _state) = harness(vec![Fixture::Sse(chunks)]).await;
    let (status, body) = call(app, anthropic_request(true)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("event: message_start"));
    assert!(body.contains("thinking_delta"));
    assert!(body.contains("Suy nghĩ"));
    assert!(body.contains("text_delta"));
    assert!(body.contains("Xin chào Việt Nam"));
    assert!(body.contains("event: message_stop"));
    assert!(body.find("thinking_delta").unwrap() < body.find("text_delta").unwrap());
}

#[tokio::test]
async fn reasoning_success_phrase_stays_before_final_text_with_tools_available() {
    let wire = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"The command executed successfully and returned RAW_BOUNDARY_BASH_OK.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"RAW_BOUNDARY_BASH_DONE\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    )
    .as_bytes()
    .to_vec();
    let (app, _state) = harness(vec![Fixture::Sse(vec![wire])]).await;
    let mut request = anthropic_request(true);
    request["tools"] = json!([{
        "name":"Bash",
        "description":"Run a harmless command",
        "input_schema":{"type":"object","properties":{"command":{"type":"string"}}}
    }]);

    let (status, body) = call(app, request).await;

    assert_eq!(status, StatusCode::OK);
    let reasoning_pos = body
        .find("The command executed successfully")
        .expect("reasoning delta missing");
    let text_pos = body
        .find("RAW_BOUNDARY_BASH_DONE")
        .expect("final text delta missing");
    assert!(
        reasoning_pos < text_pos,
        "reasoning reordered after text: {body}"
    );
    assert!(body.contains("\"stop_reason\":\"end_turn\""));
    assert_eq!(body.matches("event: message_stop").count(), 1);
}

#[tokio::test]
async fn streaming_native_tool_arguments_can_be_fragmented() {
    let chunks = vec![
        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"pa\"}}]},\"finish_reason\":null}]}\n\n".to_vec(),
        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"README.md\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n".to_vec(),
        b"data: [DONE]\n\n".to_vec(),
    ];
    let (app, _state) = harness(vec![Fixture::Sse(chunks)]).await;
    let mut request = anthropic_request(true);
    request["tools"] = json!([{
        "name":"Read",
        "description":"Read a file",
        "input_schema":{"type":"object","properties":{"path":{"type":"string"}}}
    }]);
    let (status, body) = call(app, request).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("\"type\":\"tool_use\""));
    assert!(body.contains("\"name\":\"Read\""));
    assert!(body.contains("input_json_delta"));
    assert!(body.contains("README.md"));
    assert!(body.contains("\"stop_reason\":\"tool_use\""));
    assert_eq!(
        body.matches("event: content_block_stop").count(),
        1,
        "a native tool_use block must be closed exactly once: {body}"
    );
}

#[tokio::test]
async fn streaming_native_tool_identity_fragments_and_cumulative_arguments_reassemble() {
    let chunks = vec![
        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_\",\"function\":{\"name\":\"Ba\",\"arguments\":\"{\\\"command\\\":\\\"printf \"}}]},\"finish_reason\":null}]}\n\n".to_vec(),
        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"bash\",\"function\":{\"name\":\"sh\",\"arguments\":\"{\\\"command\\\":\\\"printf fragmented\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n".to_vec(),
        b"data: [DONE]\n\n".to_vec(),
    ];
    let (app, state) = harness(vec![Fixture::Sse(chunks)]).await;
    let mut request = anthropic_request(true);
    request["tools"] = json!([{
        "name":"Bash",
        "description":"Run a command",
        "input_schema":{"type":"object","properties":{"command":{"type":"string"}}}
    }]);

    let (status, body) = call(app, request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.matches("\"type\":\"tool_use\"").count(), 1, "{body}");
    assert!(body.contains("\"id\":\"call_bash\""), "{body}");
    assert!(body.contains("\"name\":\"Bash\""), "{body}");
    assert!(body.contains("printf fragmented"), "{body}");
    assert!(!body.contains("BaBash"), "{body}");
    assert!(!body.contains("call_call_bash"), "{body}");
    assert!(body.contains("\"stop_reason\":\"tool_use\""), "{body}");
    assert_eq!(state.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn malformed_lines_and_duplicate_done_do_not_break_finalization() {
    let chunks = vec![
        b"event: ignored\ndata: {not-json}\n\n".to_vec(),
        b"data: {\"choices\":[{\"delta\":{\"content\":\"valid\"},\"finish_reason\":\"stop\"}]}\n\n"
            .to_vec(),
        b"data: [DONE]\n\ndata: [DONE]\n\n".to_vec(),
    ];
    let (app, _state) = harness(vec![Fixture::Sse(chunks)]).await;
    let (status, body) = call(app, anthropic_request(true)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("valid"));
    assert_eq!(body.matches("event: message_stop").count(), 1);
}

#[tokio::test]
async fn pre_content_stream_read_error_retries_without_duplicate_lifecycle() {
    let success = vec![
        br#"data: {"choices":[{"delta":{"content":"RECOVERED"},"finish_reason":"stop"}]}

"#
        .to_vec(),
        b"data: [DONE]

"
        .to_vec(),
    ];
    let (app, state) = harness(vec![Fixture::SseReadError(vec![]), Fixture::Sse(success)]).await;
    let (status, body) = call(app, anthropic_request(true)).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("RECOVERED"), "{body}");
    assert!(!body.contains("event: error"), "{body}");
    assert_eq!(body.matches("event: message_start").count(), 1, "{body}");
    assert_eq!(body.matches("event: message_stop").count(), 1, "{body}");
    assert_eq!(state.requests.lock().await.len(), 2);
}

#[tokio::test]
async fn post_content_stream_read_error_is_not_replayed() {
    let partial = vec![
        br#"data: {"choices":[{"delta":{"content":"VISIBLE_ONCE"},"finish_reason":null}]}

"#
        .to_vec(),
    ];
    let unused_success = vec![
        br#"data: {"choices":[{"delta":{"content":"MUST_NOT_REPLAY"},"finish_reason":"stop"}]}

"#
        .to_vec(),
        b"data: [DONE]

"
        .to_vec(),
    ];
    let (app, state) = harness(vec![
        Fixture::SseReadError(partial),
        Fixture::Sse(unused_success),
    ])
    .await;
    let (status, body) = call(app, anthropic_request(true)).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("VISIBLE_ONCE"), "{body}");
    assert!(!body.contains("MUST_NOT_REPLAY"), "{body}");
    assert_eq!(body.matches("VISIBLE_ONCE").count(), 1, "{body}");
    assert_eq!(body.matches("event: message_start").count(), 1, "{body}");
    // A mid-stream failure is reported via the terminal error event, not a
    // clean end_turn: message_stop must never follow an error event.
    assert!(body.contains("event: error"), "{body}");
    assert_eq!(body.matches("event: message_stop").count(), 0, "{body}");
    assert_eq!(state.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn compat_retry_after_visible_text_does_not_merge_second_response() {
    let broken = vec![
        br#"data: {"choices":[{"delta":{"content":"VISIBLE_BEFORE_BROKEN_MARKER "},"finish_reason":null}]}

"#
        .to_vec(),
        br#"data: {"choices":[{"delta":{"content":"[Requesting Read with arguments: {\"file_path\": \"x\"},"},"finish_reason":null}]}

"#
        .to_vec(),
        b"data: [DONE]

"
        .to_vec(),
    ];
    let success = vec![
        br#"data: {"choices":[{"delta":{"content":"SECOND_RESPONSE_TEXT"},"finish_reason":"stop"}]}

"#
        .to_vec(),
        b"data: [DONE]

"
        .to_vec(),
    ];
    let (app, state) = harness(vec![Fixture::Sse(broken), Fixture::Sse(success)]).await;
    let (status, body) = call(app, anthropic_request(true)).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("VISIBLE_BEFORE_BROKEN_MARKER"), "{body}");
    assert_eq!(
        body.matches("VISIBLE_BEFORE_BROKEN_MARKER").count(),
        1,
        "{body}"
    );
    assert!(!body.contains("SECOND_RESPONSE_TEXT"), "{body}");
    assert_eq!(body.matches("event: message_start").count(), 1, "{body}");
    assert_eq!(body.matches("event: message_stop").count(), 1, "{body}");
    assert_eq!(state.requests.lock().await.len(), 1, "{body}");
}

#[tokio::test]
async fn upstream_non_2xx_emits_single_error_after_message_start() {
    let fixture = Fixture::Raw {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        content_type: "application/json",
        chunks: vec![br#"{"error":"boom"}"#.to_vec()],
    };
    let (app, _state) = harness(vec![fixture]).await;
    let (status, body) = call(app, anthropic_request(true)).await;

    assert_eq!(status, StatusCode::OK);
    let start_pos = body
        .find("event: message_start")
        .expect("message_start must be present");
    let first_error_pos = body.find("event: error").expect("error must be present");
    assert!(
        start_pos < first_error_pos,
        "message_start must precede the error event: {body}"
    );
    assert_eq!(body.matches("event: error").count(), 1, "{body}");
    // The error event is terminal: no message_delta or message_stop may follow.
    assert_eq!(body.matches("event: message_delta").count(), 0, "{body}");
    assert_eq!(body.matches("event: message_stop").count(), 0, "{body}");
}

#[tokio::test]
async fn malformed_anthropic_body_returns_anthropic_error_shape() {
    let (app, _state) = harness(vec![]).await;
    let (status, body) = call(app, json!({"messages": "oops"})).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("\"type\":\"error\""), "{body}");
    assert!(body.contains("\"error\""), "{body}");
    assert!(body.contains("\"message\""), "{body}");
}

#[tokio::test]
async fn post_content_failure_with_pending_tool_call_emits_error_not_clean_end_turn() {
    let partial = vec![
        br#"data: {"choices":[{"delta":{"content":"VISIBLE_PARTIAL "},"finish_reason":null}]}

"#
        .to_vec(),
        br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"Read","arguments":"{\"file_path\":\"re"}}]},"finish_reason":null}]}

"#
        .to_vec(),
    ];
    let (app, _state) = harness(vec![Fixture::SseReadError(partial)]).await;
    let (status, body) = call(app, anthropic_request(true)).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("VISIBLE_PARTIAL"), "{body}");
    // The client must be told the message is incomplete rather than receiving a
    // clean end_turn that implies the tool call was not executed when it was
    // never completed. The dropped tool call must never be emitted partially,
    // and the terminal error event carries no message_stop after it.
    assert!(body.contains("event: error"), "{body}");
    assert!(!body.contains("\"name\":\"Read\""), "{body}");
    assert_eq!(body.matches("event: message_stop").count(), 0, "{body}");
}

#[tokio::test]
async fn premature_eof_still_emits_one_terminal_message_stop() {
    let chunks = vec![
        b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n"
            .to_vec(),
    ];
    let (app, _state) = harness(vec![Fixture::Sse(chunks)]).await;
    let (status, body) = call(app, anthropic_request(true)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("partial"));
    assert_eq!(body.matches("event: message_stop").count(), 1);
}

#[tokio::test]
async fn upstream_non_json_sync_response_maps_to_safe_error() {
    let fixture = Fixture::Raw {
        status: StatusCode::OK,
        content_type: "text/plain",
        chunks: vec![b"not-json".to_vec()],
    };
    let (app, _state) = harness(vec![fixture]).await;
    let (status, body) = call(app, anthropic_request(false)).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body.contains("error"));
    assert!(!body.contains("backtrace"));
}

#[tokio::test]
async fn oversized_sync_body_is_rejected_before_json_allocation() {
    let oversized = format!(
        "{{\"id\":\"x\",\"model\":\"x\",\"choices\":[],\"padding\":\"{}\"}}",
        "x".repeat(4096)
    );
    let fixture = Fixture::Raw {
        status: StatusCode::OK,
        content_type: "application/json",
        chunks: vec![oversized.into_bytes()],
    };
    let (app, _state) = harness_with_limits(vec![fixture], 4096, 1024).await;
    let (status, body) = call(app, anthropic_request(false)).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body.contains("configured limit"));
    assert!(!body.contains(&"x".repeat(100)));
}

#[tokio::test]
async fn oversized_sse_line_emits_terminal_error_without_message_stop() {
    let huge = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}},\"finish_reason\":null}}]}}\n\n",
        "x".repeat(4096)
    );
    let (app, _state) =
        harness_with_limits(vec![Fixture::Sse(vec![huge.into_bytes()])], 1024, 4096).await;
    let (status, body) = call(app, anthropic_request(true)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("event: error"));
    assert!(body.contains("exceeded configured byte limit"));
    // The error event is terminal — a single error, no message_stop after it.
    assert_eq!(body.matches("event: error").count(), 1, "{body}");
    assert_eq!(body.matches("event: message_delta").count(), 0, "{body}");
    assert_eq!(body.matches("event: message_stop").count(), 0, "{body}");
}

#[tokio::test]
async fn aggregate_sse_chunk_can_exceed_limit_when_each_line_is_valid() {
    let lines = [
        b"data: {\"choices\":[{\"delta\":{\"content\":\"A\"},\"finish_reason\":null}]}\n"
            .as_slice(),
        b"data: {\"choices\":[{\"delta\":{\"content\":\"B\"},\"finish_reason\":null}]}\n"
            .as_slice(),
        b"data: {\"choices\":[{\"delta\":{\"content\":\"C\"},\"finish_reason\":\"stop\"}]}\n"
            .as_slice(),
        b"data: [DONE]\n\n".as_slice(),
    ];
    let max_line_bytes = 128;
    assert!(lines.iter().all(|line| line.len() <= max_line_bytes));
    let chunk = lines.concat();
    assert!(chunk.len() > max_line_bytes);

    let (app, _state) =
        harness_with_limits(vec![Fixture::Sse(vec![chunk])], max_line_bytes, 4096).await;
    let (status, body) = call(app, anthropic_request(true)).await;

    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("event: error"), "{body}");
    assert!(body.contains("\"text\":\"A\""), "{body}");
    assert!(body.contains("\"text\":\"B\""));
    assert!(body.contains("\"text\":\"C\""));
    assert_eq!(body.matches("event: message_stop").count(), 1);
}

struct UpstreamDropNotify(Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for UpstreamDropNotify {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

fn cancellable_response(sender: tokio::sync::oneshot::Sender<()>) -> Response<Body> {
    let body = Body::from_stream(async_stream::stream! {
        let _drop_notify = UpstreamDropNotify(Some(sender));
        yield Ok::<Bytes, Infallible>(Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"first\"},\"finish_reason\":null}]}\n\n"
        ));
        futures_util::future::pending::<()>().await;
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(body)
        .unwrap()
}

#[tokio::test]
async fn dropping_client_stream_cancels_upstream_body() {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let (app, _state, metrics) = harness_core(
        vec![Fixture::Cancellable(sender)],
        256 * 1024,
        4 * 1024 * 1024,
        |_| {},
    )
    .await;
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&anthropic_request(true)).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut stream = response.into_body().into_data_stream();
    let first = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
        .await
        .expect("first downstream event timeout")
        .expect("first downstream event")
        .expect("first downstream bytes");
    assert!(!first.is_empty());
    drop(stream);
    tokio::time::timeout(std::time::Duration::from_secs(2), receiver)
        .await
        .expect("upstream stream was not dropped after client disconnect")
        .expect("drop notifier sender disappeared");
    tokio::task::yield_now().await;
    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.streams_started, 1);
    assert_eq!(snapshot.streams_cancelled, 1);
    assert_eq!(snapshot.active_streams, 0);
}

#[tokio::test]
async fn provider_server_error_fails_fast_and_completed_streams_are_counted() {
    let stream_chunks = vec![
        b"data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\n"
            .to_vec(),
        b"data: [DONE]\n\n".to_vec(),
    ];
    let fixtures = vec![
        Fixture::Raw {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            content_type: "text/plain",
            chunks: vec![b"temporary".to_vec()],
        },
        Fixture::Sse(stream_chunks),
    ];
    let (app, _state, metrics) = harness_core(fixtures, 256 * 1024, 4 * 1024 * 1024, |config| {
        config.retry.base_backoff = std::time::Duration::ZERO;
        config.retry.max_backoff = std::time::Duration::ZERO;
        config.retry.max_network_attempts = 5;
    })
    .await;

    // A 5xx is a provider-side transient: the bridge fails fast (after the
    // model-fallback chain) instead of sleep-backoff retrying — the client's
    // own retry loop is what recovers from it. No provider retry is recorded.
    let (status, body) = call(app.clone(), anthropic_request(false)).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body.contains("error"), "{body}");
    let (status, body) = call(app, anthropic_request(true)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("done"));

    tokio::task::yield_now().await;
    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.retry_provider_server, 0);
    assert_eq!(snapshot.streams_started, 1);
    assert_eq!(snapshot.streams_completed, 1);
    assert_eq!(snapshot.active_streams, 0);
}

#[tokio::test]
async fn configured_model_fallback_is_counted_separately_from_retry() {
    let success = json!({
        "id":"chatcmpl-fallback",
        "model":"fallback-model",
        "choices":[{
            "message":{"content":"fallback ok","reasoning_content":null,"tool_calls":null},
            "finish_reason":"stop"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1}
    });
    let fixtures = vec![
        Fixture::Raw {
            status: StatusCode::TOO_MANY_REQUESTS,
            content_type: "text/plain",
            chunks: vec![b"rate limited".to_vec()],
        },
        Fixture::Json(success),
    ];
    let (app, state, metrics) = harness_core(fixtures, 256 * 1024, 4 * 1024 * 1024, |config| {
        config.retry.model_fallbacks = vec!["fallback-model".to_string()];
        config.retry.base_backoff = std::time::Duration::ZERO;
        config.retry.max_backoff = std::time::Duration::ZERO;
    })
    .await;
    let (status, body) = call(app, anthropic_request(false)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("fallback ok"));

    let requests = state.requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["model"], "fixture-model");
    assert_eq!(requests[1]["model"], "fallback-model");
    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.model_fallbacks, 1);
    assert_eq!(snapshot.retry_rate_limit, 0);
}

#[tokio::test]
async fn sync_malformed_compat_marker_retries_then_emits_one_tool_use() {
    let malformed = json!({
        "id":"bad-compat",
        "model":"upstream-model",
        "choices":[{
            "message":{
                "content":"[Requesting Read with arguments: {\"path\":]",
                "reasoning_content":null,
                "tool_calls":null
            },
            "finish_reason":"stop"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1}
    });
    let valid = json!({
        "id":"good-compat",
        "model":"upstream-model",
        "choices":[{
            "message":{
                "content":"[Requesting Read with arguments: {\"path\":\"README.md\"}]",
                "reasoning_content":null,
                "tool_calls":null
            },
            "finish_reason":"stop"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1}
    });
    let (app, state) = harness(vec![Fixture::Json(malformed), Fixture::Json(valid)]).await;
    let mut request = anthropic_request(false);
    request["tools"] = json!([{
        "name":"Read",
        "description":"Read a file",
        "input_schema":{"type":"object","properties":{"path":{"type":"string"}}}
    }]);

    let (status, body) = call(app, request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["content"].as_array().unwrap().len(), 1);
    assert_eq!(response["content"][0]["type"], "tool_use");
    assert_eq!(response["content"][0]["name"], "Read");
    assert_eq!(response["content"][0]["input"]["path"], "README.md");
    assert_eq!(state.requests.lock().await.len(), 2);
    let retry_request = state.requests.lock().await[1].clone();
    let retry_system = retry_request["messages"][0]["content"]
        .as_str()
        .unwrap_or_default();
    assert!(retry_system.contains("Read") || retry_request.to_string().contains("Read"));
}

#[tokio::test]
async fn sync_fenced_compat_and_dsml_examples_remain_text() {
    let example = concat!(
        "```text\n",
        "[Requesting Read with arguments: {\"path\":\"secret\"}]\n",
        "<｜DSML｜tool_calls><｜DSML｜invoke name=\"Read\"><｜DSML｜parameter name=\"path\">secret</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>\n",
        "```"
    );
    let fixture = json!({
        "id":"code-example",
        "model":"upstream-model",
        "choices":[{
            "message":{"content":example,"reasoning_content":null,"tool_calls":null},
            "finish_reason":"stop"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1}
    });
    let (app, state) = harness(vec![Fixture::Json(fixture)]).await;
    let mut request = anthropic_request(false);
    request["tools"] = json!([{
        "name":"Read",
        "description":"Read a file",
        "input_schema":{"type":"object","properties":{"path":{"type":"string"}}}
    }]);

    let (status, body) = call(app, request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["stop_reason"], "end_turn");
    assert_eq!(response["content"].as_array().unwrap().len(), 1);
    assert_eq!(response["content"][0]["type"], "text");
    assert_eq!(response["content"][0]["text"], example);
    assert_eq!(state.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn streaming_malformed_native_arguments_retry_without_partial_tool_use() {
    let bad = vec![
        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"bad\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"path\\\":\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n".to_vec(),
        b"data: [DONE]\n\n".to_vec(),
    ];
    let good = vec![
        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"good\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n".to_vec(),
        b"data: [DONE]\n\n".to_vec(),
    ];
    let (app, state) = harness(vec![Fixture::Sse(bad), Fixture::Sse(good)]).await;
    let mut request = anthropic_request(true);
    request["tools"] = json!([{
        "name":"Read",
        "description":"Read a file",
        "input_schema":{"type":"object","properties":{"path":{"type":"string"}}}
    }]);

    let (status, body) = call(app, request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.matches("\"type\":\"tool_use\"").count(), 1, "{body}");
    assert!(body.contains("README.md"), "{body}");
    assert!(!body.contains("\"id\":\"bad\""), "{body}");
    assert_eq!(state.requests.lock().await.len(), 2);
}

#[tokio::test]
async fn streaming_native_tool_without_finish_reason_finalizes_at_done() {
    let chunks = vec![
        b"data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"done-call\",\"function\":{\"name\":\"Read\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}]},\"finish_reason\":null}]}\n\n".to_vec(),
        b"data: [DONE]\n\n".to_vec(),
    ];
    let (app, state) = harness(vec![Fixture::Sse(chunks)]).await;
    let mut request = anthropic_request(true);
    request["tools"] = json!([{
        "name":"Read",
        "description":"Read a file",
        "input_schema":{"type":"object","properties":{"path":{"type":"string"}}}
    }]);

    let (status, body) = call(app, request).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("\"type\":\"tool_use\""), "{body}");
    assert!(body.contains("done-call"), "{body}");
    assert!(body.contains("README.md"), "{body}");
    assert!(body.contains("\"stop_reason\":\"tool_use\""), "{body}");
    assert_eq!(state.requests.lock().await.len(), 1);
}

fn cron_tool_request(stream: bool) -> Value {
    let mut request = anthropic_request(stream);
    request["tools"] = json!([{
        "name":"CronCreate",
        "description":"Create a session cron",
        "input_schema":{
            "type":"object",
            "properties":{
                "cron":{"type":"string"},
                "prompt":{"type":"string"},
                "recurring":{"type":"boolean"}
            },
            "required":["cron","prompt"]
        }
    }]);
    request
}

#[tokio::test]
async fn streaming_direct_cron_marker_is_one_tool_block_without_leak() {
    let marker = "[Requesting CronCreate: {\"cron\":\"*/30 * * * *\",\"prompt\":\"write CRON_PARSE_VERIFY_OK\",\"recurring\":true}]";
    let wire = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "choices":[{
                "delta":{"content":marker},
                "finish_reason":"stop"
            }]
        })
    )
    .into_bytes();
    let chunks = wire.chunks(3).map(<[u8]>::to_vec).collect::<Vec<_>>();
    let retry_chunks = chunks.clone();
    let (app, state) = harness(vec![Fixture::Sse(chunks), Fixture::Sse(retry_chunks)]).await;

    let (status, body) = call(app, cron_tool_request(true)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.matches("\"type\":\"tool_use\"").count(), 1, "{body}");
    assert_eq!(body.matches("\"name\":\"CronCreate\"").count(), 1, "{body}");
    assert_eq!(
        body.matches("\"type\":\"content_block_start\"").count(),
        1,
        "{body}"
    );
    assert_eq!(
        body.matches("\"type\":\"content_block_stop\"").count(),
        1,
        "{body}"
    );
    assert_eq!(body.matches("toolu_compat_").count(), 1, "{body}");
    assert!(!body.contains("Requesting CronCreate"), "{body}");
    assert!(body.contains("CRON_PARSE_VERIFY_OK"), "{body}");
    assert_eq!(state.requests.lock().await.len(), 2);
}

#[tokio::test]
async fn streaming_reasoning_and_text_duplicate_cron_executes_once() {
    let marker = "[Requesting CronCreate: {\"cron\":\"*/30 * * * *\",\"prompt\":\"verify\",\"recurring\":true}]";
    let events = [
        json!({"choices":[{"delta":{"reasoning_content":marker},"finish_reason":null}]}),
        json!({"choices":[{"delta":{"content":marker},"finish_reason":"stop"}]}),
    ];
    let wire = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        events[0], events[1]
    )
    .into_bytes();
    let chunks = wire.chunks(7).map(<[u8]>::to_vec).collect::<Vec<_>>();
    let retry_chunks = chunks.clone();
    let (app, state) = harness(vec![Fixture::Sse(chunks), Fixture::Sse(retry_chunks)]).await;

    let (status, body) = call(app, cron_tool_request(true)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.matches("\"type\":\"tool_use\"").count(), 1, "{body}");
    assert_eq!(body.matches("\"name\":\"CronCreate\"").count(), 1, "{body}");
    assert!(!body.contains("Requesting CronCreate"), "{body}");
    assert_eq!(
        body.matches("\"stop_reason\":\"tool_use\"").count(),
        1,
        "{body}"
    );
    assert_eq!(state.requests.lock().await.len(), 2);
}

#[tokio::test]
async fn sync_reasoning_marker_and_false_success_text_emit_only_tool_use() {
    let marker = "[Requesting CronCreate: {\"cron\":\"*/30 * * * *\",\"prompt\":\"verify\",\"recurring\":true}]";
    let fixture = json!({
        "id":"sync-cron-reasoning",
        "model":"upstream-model",
        "choices":[{
            "message":{
                "content":format!("Cron đã được tạo thành công.\n{marker}"),
                "reasoning_content":marker,
                "tool_calls":null
            },
            "finish_reason":"stop"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1}
    });
    let retry_fixture = fixture.clone();
    let (app, state) = harness(vec![Fixture::Json(fixture), Fixture::Json(retry_fixture)]).await;

    let (status, body) = call(app, cron_tool_request(false)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let response: Value = serde_json::from_str(&body).unwrap();
    let content = response["content"].as_array().unwrap();
    assert_eq!(content.len(), 1, "{response}");
    assert_eq!(content[0]["type"], "tool_use");
    assert_eq!(content[0]["name"], "CronCreate");
    assert_eq!(content[0]["input"]["cron"], "*/30 * * * *");
    assert_eq!(response["stop_reason"], "tool_use");
    assert!(!body.contains("Requesting CronCreate"), "{body}");
    assert!(!body.contains("tạo thành công"), "{body}");
    assert_eq!(state.requests.lock().await.len(), 2);
}

#[tokio::test]
async fn sync_generic_xml_bash_marker_maps_to_one_tool_use() {
    let marker = concat!(
        "<tool_calls><invoke name=\"Bash\">",
        "<parameter name=\"command\">printf GENERIC_XML_BASH_OK</parameter>",
        "<parameter name=\"description\">Verify generic XML Bash</parameter>",
        "</invoke></tool_calls></think>"
    );
    let fixture = json!({
        "id":"generic-xml-sync",
        "model":"upstream-model",
        "choices":[{
            "message":{"content":marker,"reasoning_content":null,"tool_calls":null},
            "finish_reason":"stop"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1}
    });
    let retry_fixture = fixture.clone();
    let (app, state) = harness(vec![Fixture::Json(fixture), Fixture::Json(retry_fixture)]).await;
    let mut request = anthropic_request(false);
    request["tools"] = json!([{
        "name":"Bash",
        "description":"Run a command",
        "input_schema":{
            "type":"object",
            "properties":{
                "command":{"type":"string"},
                "description":{"type":"string"}
            },
            "required":["command"]
        }
    }]);

    let (status, body) = call(app, request).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["stop_reason"], "tool_use");
    assert_eq!(response["content"].as_array().unwrap().len(), 1);
    assert_eq!(response["content"][0]["type"], "tool_use");
    assert_eq!(response["content"][0]["name"], "Bash");
    assert_eq!(
        response["content"][0]["input"]["command"],
        "printf GENERIC_XML_BASH_OK"
    );
    assert_eq!(
        response["content"][0]["input"]["description"],
        "Verify generic XML Bash"
    );
    assert!(!body.contains("tool_calls"), "{body}");
    assert!(!body.contains("<invoke"), "{body}");
    assert_eq!(state.requests.lock().await.len(), 2);
}

#[tokio::test]
async fn streaming_generic_xml_agent_marker_is_fragment_safe_and_exact_once() {
    let marker = concat!(
        "<tool_calls><invoke name=\"Agent\">",
        "<parameter name=\"description\">Review parser recovery</parameter>",
        "<parameter name=\"prompt\">Inspect the patch and return evidence.</parameter>",
        "<parameter name=\"subagent_type\">general-purpose</parameter>",
        "</invoke></tool_calls>"
    );
    let mut chunks = Vec::new();
    for part in marker.as_bytes().chunks(13) {
        let content = std::str::from_utf8(part).unwrap();
        chunks.push(
            format!(
                "data: {}\n\n",
                json!({
                    "choices":[{
                        "delta":{"content":content},
                        "finish_reason":null
                    }]
                })
            )
            .into_bytes(),
        );
    }
    chunks.push(
        format!(
            "data: {}\n\n",
            json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
        )
        .into_bytes(),
    );
    chunks.push(b"data: [DONE]\n\n".to_vec());
    let retry_chunks = chunks.clone();
    let (app, state) = harness(vec![Fixture::Sse(chunks), Fixture::Sse(retry_chunks)]).await;
    let mut request = anthropic_request(true);
    request["tools"] = json!([{
        "name":"Agent",
        "description":"Spawn an agent",
        "input_schema":{
            "type":"object",
            "properties":{
                "description":{"type":"string"},
                "prompt":{"type":"string"},
                "subagent_type":{"type":"string"}
            },
            "required":["description","prompt"]
        }
    }]);

    let (status, body) = call(app, request).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.matches("\"type\":\"tool_use\"").count(), 1, "{body}");
    assert_eq!(body.matches("\"name\":\"Agent\"").count(), 1, "{body}");
    assert_eq!(
        body.matches("event: content_block_stop").count(),
        1,
        "{body}"
    );
    assert!(body.contains("Review parser recovery"), "{body}");
    assert!(body.contains("general-purpose"), "{body}");
    assert!(body.contains("\"stop_reason\":\"tool_use\""), "{body}");
    assert!(!body.contains("tool_calls"), "{body}");
    assert!(!body.contains("<invoke"), "{body}");
    assert_eq!(state.requests.lock().await.len(), 2);
}

#[tokio::test]
async fn streaming_agent_compat_recovers_after_native_retry_fails() {
    let marker = r#"[Requesting Agent with arguments: {"description":"Compat fanout","prompt":"Inspect parser recovery and return evidence.","subagent_type":"general-purpose"}]"#;
    let response = || {
        vec![
            format!(
                "data: {}\n\n",
                json!({
                    "choices":[{
                        "delta":{"content":marker},
                        "finish_reason":"stop"
                    }]
                })
            )
            .into_bytes(),
            b"data: [DONE]\n\n".to_vec(),
        ]
    };
    let (app, state) = harness(vec![Fixture::Sse(response()), Fixture::Sse(response())]).await;
    let mut request = anthropic_request(true);
    request["tools"] = json!([{
        "name":"Agent",
        "description":"Spawn an agent",
        "input_schema":{
            "type":"object",
            "properties":{
                "description":{"type":"string"},
                "prompt":{"type":"string"},
                "subagent_type":{"type":"string"}
            },
            "required":["description","prompt"]
        }
    }]);

    let (status, body) = call(app, request).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(state.requests.lock().await.len(), 2);
    assert_eq!(body.matches("\"type\":\"tool_use\"").count(), 1, "{body}");
    assert_eq!(body.matches("\"name\":\"Agent\"").count(), 1, "{body}");
    assert!(body.contains("Compat fanout"), "{body}");
    assert!(
        body.contains("Inspect parser recovery and return evidence."),
        "{body}"
    );
    assert!(!body.contains("Requesting Agent"), "{body}");
    assert!(body.contains("\"stop_reason\":\"tool_use\""), "{body}");
}

#[tokio::test]
async fn streaming_encoded_candidate_retries_native_before_tool_use() {
    let encoded_marker = r#"[Requesting Tool execution: 'Bash' with arguments: {"command":"printf ENCODED_SHOULD_NOT_EXECUTE"}]"#;
    let first = vec![
        format!(
            "data: {}\n\n",
            json!({
                "choices":[{
                    "delta":{"content":encoded_marker},
                    "finish_reason":"stop"
                }]
            })
        )
        .into_bytes(),
        b"data: [DONE]\n\n".to_vec(),
    ];
    let native = vec![
        format!(
            "data: {}\n\n",
            json!({
                "choices":[{
                    "delta":{"tool_calls":[{
                        "index":0,
                        "id":"call_native_recovery",
                        "function":{
                            "name":"Bash",
                            "arguments":"{\"command\":\"printf NATIVE_RECOVERY_OK\"}"
                        }
                    }]},
                    "finish_reason":"tool_calls"
                }]
            })
        )
        .into_bytes(),
        b"data: [DONE]\n\n".to_vec(),
    ];
    let (app, state) = harness(vec![Fixture::Sse(first), Fixture::Sse(native)]).await;
    let mut request = anthropic_request(true);
    request["tools"] = json!([{
        "name":"Bash",
        "description":"Run a command",
        "input_schema":{
            "type":"object",
            "properties":{"command":{"type":"string"}},
            "required":["command"]
        }
    }]);

    let (status, body) = call(app, request).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(state.requests.lock().await.len(), 2);
    assert_eq!(body.matches("\"type\":\"tool_use\"").count(), 1, "{body}");
    assert!(body.contains("NATIVE_RECOVERY_OK"), "{body}");
    assert!(!body.contains("ENCODED_SHOULD_NOT_EXECUTE"), "{body}");
    assert_eq!(body.matches("event: message_start").count(), 1, "{body}");
    assert_eq!(body.matches("event: message_stop").count(), 1, "{body}");
}

#[tokio::test]
async fn streaming_native_call_wins_over_duplicate_encoded_marker_after_recovery() {
    let first_marker =
        r#"[Requesting Tool execution: 'Bash' with arguments: {"command":"printf NATIVE_WINS"}]"#;
    let first = vec![
        format!(
            "data: {}\n\n",
            json!({"choices":[{"delta":{"content":first_marker},"finish_reason":"stop"}]})
        )
        .into_bytes(),
        b"data: [DONE]\n\n".to_vec(),
    ];
    let duplicate_and_native = vec![
        format!(
            "data: {}\n\n",
            json!({
                "choices":[{
                    "delta":{
                        "content":first_marker,
                        "tool_calls":[{
                            "index":0,
                            "id":"call_native_wins",
                            "function":{
                                "name":"Bash",
                                "arguments":"{\"command\":\"printf NATIVE_WINS\"}"
                            }
                        }]
                    },
                    "finish_reason":"tool_calls"
                }]
            })
        )
        .into_bytes(),
        b"data: [DONE]\n\n".to_vec(),
    ];
    let (app, state) = harness(vec![
        Fixture::Sse(first),
        Fixture::Sse(duplicate_and_native),
    ])
    .await;
    let mut request = anthropic_request(true);
    request["tools"] = json!([{
        "name":"Bash",
        "description":"Run a command",
        "input_schema":{
            "type":"object",
            "properties":{"command":{"type":"string"}},
            "required":["command"]
        }
    }]);

    let (status, body) = call(app, request).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(state.requests.lock().await.len(), 2);
    assert_eq!(body.matches("\"type\":\"tool_use\"").count(), 1, "{body}");
    assert!(body.contains("\"id\":\"call_native_wins\""), "{body}");
    assert!(!body.contains("toolu_compat_"), "{body}");
    assert_eq!(body.matches("NATIVE_WINS").count(), 1, "{body}");
}

#[tokio::test]
async fn streaming_encoded_candidate_after_native_retry_uses_strict_fallback() {
    let first_marker = r#"[Requesting Tool execution: 'Bash' with arguments: {"command":"printf FIRST_ENCODED_MUST_NOT_EXECUTE"}]"#;
    let fallback_marker = r#"[Requesting Tool execution: 'Bash' with arguments: {"command":"printf STRICT_FALLBACK_OK"}]"#;
    let first = vec![
        format!(
            "data: {}\n\n",
            json!({"choices":[{"delta":{"content":first_marker},"finish_reason":"stop"}]})
        )
        .into_bytes(),
        b"data: [DONE]\n\n".to_vec(),
    ];
    let fallback = vec![
        format!(
            "data: {}\n\n",
            json!({"choices":[{"delta":{"content":fallback_marker},"finish_reason":"stop"}]})
        )
        .into_bytes(),
        b"data: [DONE]\n\n".to_vec(),
    ];
    let (app, state) = harness(vec![Fixture::Sse(first), Fixture::Sse(fallback)]).await;
    let mut request = anthropic_request(true);
    request["tools"] = json!([{
        "name":"Bash",
        "description":"Run a command",
        "input_schema":{
            "type":"object",
            "properties":{"command":{"type":"string"}},
            "required":["command"]
        }
    }]);

    let (status, body) = call(app, request).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(state.requests.lock().await.len(), 2);
    assert_eq!(body.matches("\"type\":\"tool_use\"").count(), 1, "{body}");
    assert!(body.contains("STRICT_FALLBACK_OK"), "{body}");
    assert!(!body.contains("FIRST_ENCODED_MUST_NOT_EXECUTE"), "{body}");
    assert_eq!(body.matches("event: message_start").count(), 1, "{body}");
    assert_eq!(body.matches("event: message_stop").count(), 1, "{body}");
}

#[tokio::test]
async fn streaming_malformed_generic_xml_retries_then_emits_one_tool_use() {
    let malformed_marker = concat!(
        "<tool_call><invoke name=\"Bash\">\n",
        "Command: printf BROKEN\n",
        "Description: malformed generic XML\n",
        "</parameter>\n</invoke>\n</tool_call></think>"
    );
    let valid_marker = concat!(
        "<tool_calls><invoke name=\"Bash\">",
        "<parameter name=\"command\">printf GENERIC_XML_RETRY_OK</parameter>",
        "<parameter name=\"description\">Recovered generic XML</parameter>",
        "</invoke></tool_calls>"
    );
    let malformed = vec![
        format!(
            "data: {}\n\n",
            json!({
                "choices":[{
                    "delta":{"content":malformed_marker},
                    "finish_reason":"stop"
                }]
            })
        )
        .into_bytes(),
        b"data: [DONE]\n\n".to_vec(),
    ];
    let valid = vec![
        format!(
            "data: {}\n\n",
            json!({
                "choices":[{
                    "delta":{"content":valid_marker},
                    "finish_reason":"stop"
                }]
            })
        )
        .into_bytes(),
        b"data: [DONE]\n\n".to_vec(),
    ];
    let strict_fallback = valid.clone();
    let (app, state) = harness(vec![
        Fixture::Sse(malformed),
        Fixture::Sse(valid),
        Fixture::Sse(strict_fallback),
    ])
    .await;
    let mut request = anthropic_request(true);
    request["tools"] = json!([{
        "name":"Bash",
        "description":"Run a command",
        "input_schema":{
            "type":"object",
            "properties":{
                "command":{"type":"string"},
                "description":{"type":"string"}
            },
            "required":["command"]
        }
    }]);

    let (status, body) = call(app, request).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.matches("\"type\":\"tool_use\"").count(), 1, "{body}");
    assert_eq!(body.matches("event: message_start").count(), 1, "{body}");
    assert!(body.contains("GENERIC_XML_RETRY_OK"), "{body}");
    assert!(!body.contains("BROKEN"), "{body}");
    assert!(!body.contains("tool_call"), "{body}");
    assert!(!body.contains("<invoke"), "{body}");
    assert_eq!(state.requests.lock().await.len(), 3);
}

#[tokio::test]
async fn sync_fenced_generic_xml_example_remains_inert_text() {
    let example = concat!(
        "```xml\n",
        "<tool_calls><invoke name=\"Bash\">",
        "<parameter name=\"command\">printf MUST_NOT_RUN</parameter>",
        "</invoke></tool_calls>\n",
        "```"
    );
    let fixture = json!({
        "id":"generic-xml-code-example",
        "model":"upstream-model",
        "choices":[{
            "message":{"content":example,"reasoning_content":null,"tool_calls":null},
            "finish_reason":"stop"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1}
    });
    let (app, state) = harness(vec![Fixture::Json(fixture)]).await;
    let mut request = anthropic_request(false);
    request["tools"] = json!([{
        "name":"Bash",
        "description":"Run a command",
        "input_schema":{"type":"object","properties":{"command":{"type":"string"}}}
    }]);

    let (status, body) = call(app, request).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["stop_reason"], "end_turn");
    assert_eq!(response["content"][0]["type"], "text");
    assert_eq!(response["content"][0]["text"], example);
    assert_eq!(state.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn claude_code_wrapped_bang_command_bypasses_upstream_and_emits_bash_tool() {
    let (app, state, _metrics) = harness_core(Vec::new(), 256 * 1024, 4 * 1024 * 1024, |config| {
        config.shell_policy = ShellPolicy::Unrestricted
    })
    .await;
    let request = json!({
        "model": "fixture-model",
        "messages": [{
            "role": "user",
            "content": concat!(
                "<system-reminder>Available agent types...</system-reminder>\n",
                "<system-reminder>Available skills...</system-reminder>\n\n",
                "!printf PTY_SHELL_OK"
            )
        }],
        "tools": [{
            "name": "Bash",
            "description": "Run a shell command",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "description": {"type": "string"}
                },
                "required": ["command"]
            }
        }],
        "stream": true,
        "max_tokens": 128
    });

    let (status, body) = call(app, request).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.matches("\"type\":\"tool_use\"").count(), 1, "{body}");
    assert_eq!(body.matches("\"name\":\"Bash\"").count(), 1, "{body}");
    assert!(body.contains("printf PTY_SHELL_OK"), "{body}");
    assert!(!body.contains("system-reminder"), "{body}");
    assert!(body.contains("\"stop_reason\":\"tool_use\""), "{body}");
    assert!(state.requests.lock().await.is_empty());
}

#[tokio::test]
async fn sync_agent_placeholder_retries_then_emits_nonempty_prompt() {
    let placeholder = concat!(
        "<tool_calls><invoke name=\"Agent\">",
        "<parameter name=\"description\">Re-review Task 1</parameter>",
        "<parameter name=\"prompt\">...</parameter>",
        "</invoke></tool_calls>"
    );
    let valid = concat!(
        "<tool_calls><invoke name=\"Agent\">",
        "<parameter name=\"description\">Re-review Task 1</parameter>",
        "<parameter name=\"prompt\">Inspect the complete review package and report concrete evidence.</parameter>",
        "<parameter name=\"run_in_background\">false</parameter>",
        "</invoke></tool_calls>"
    );
    let first = json!({
        "id":"sync-agent-placeholder",
        "model":"upstream-model",
        "choices":[{
            "message":{"content":placeholder,"reasoning_content":null,"tool_calls":null},
            "finish_reason":"stop"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1}
    });
    let second = json!({
        "id":"sync-agent-valid",
        "model":"upstream-model",
        "choices":[{
            "message":{"content":valid,"reasoning_content":null,"tool_calls":null},
            "finish_reason":"stop"
        }],
        "usage":{"prompt_tokens":1,"completion_tokens":1}
    });
    let (app, state) = harness(vec![Fixture::Json(first), Fixture::Json(second)]).await;
    let mut request = anthropic_request(false);
    request["tools"] = json!([{
        "name":"Agent",
        "description":"Spawn an agent",
        "input_schema":{
            "type":"object",
            "properties":{
                "description":{"type":"string"},
                "prompt":{"type":"string"},
                "run_in_background":{"type":"boolean"}
            },
            "required":["description","prompt"]
        }
    }]);

    let (status, body) = call(app, request).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    let response: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(response["stop_reason"], "tool_use");
    assert_eq!(response["content"].as_array().unwrap().len(), 1);
    assert_eq!(response["content"][0]["name"], "Agent");
    assert_eq!(
        response["content"][0]["input"]["prompt"],
        "Inspect the complete review package and report concrete evidence."
    );
    assert_eq!(response["content"][0]["input"]["run_in_background"], false);
    assert!(!body.contains("\"prompt\":\"...\""), "{body}");
    assert_eq!(state.requests.lock().await.len(), 2);
}

#[tokio::test]
async fn large_upstream_reasoning_chunk_is_split_into_bounded_anthropic_deltas() {
    let reasoning = "lập luận Unicode tiếng Việt — ".repeat(700);
    let upstream = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({
            "choices": [{
                "delta": {"reasoning_content": reasoning},
                "finish_reason": null
            }]
        })
    );
    let (app, _state) = harness(vec![Fixture::Sse(vec![upstream.into_bytes()])]).await;
    let (status, body) = call(app, anthropic_request(true)).await;
    assert_eq!(status, StatusCode::OK);

    let mut fragments = Vec::new();
    let mut wire_lengths = Vec::new();
    for line in body.lines().filter(|line| line.starts_with("data: ")) {
        let Ok(value) = serde_json::from_str::<Value>(&line[6..]) else {
            continue;
        };
        if value["type"] == "content_block_delta" && value["delta"]["type"] == "thinking_delta" {
            fragments.push(
                value["delta"]["thinking"]
                    .as_str()
                    .expect("thinking fragment")
                    .to_string(),
            );
            wire_lengths.push(line.len());
        }
    }

    assert!(
        fragments.len() > 1,
        "a single large upstream chunk must not become one TUI-blocking delta; lengths={wire_lengths:?}"
    );
    assert!(
        wire_lengths.iter().all(|length| *length <= 2_048),
        "outgoing thinking delta exceeded render bound: {wire_lengths:?}"
    );
    assert_eq!(fragments.concat(), reasoning);
}

#[tokio::test]
async fn large_upstream_text_chunk_is_split_without_corrupting_utf8() {
    let text = "Báo cáo chuyên gia hoàn tất — dữ liệu được giữ nguyên.\n".repeat(180);
    let upstream = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({
            "choices": [{
                "delta": {"content": text},
                "finish_reason": null
            }]
        })
    );
    let (app, _state) = harness(vec![Fixture::Sse(vec![upstream.into_bytes()])]).await;
    let (status, body) = call(app, anthropic_request(true)).await;
    assert_eq!(status, StatusCode::OK);

    let mut fragments = Vec::new();
    let mut wire_lengths = Vec::new();
    for line in body.lines().filter(|line| line.starts_with("data: ")) {
        let Ok(value) = serde_json::from_str::<Value>(&line[6..]) else {
            continue;
        };
        if value["type"] == "content_block_delta" && value["delta"]["type"] == "text_delta" {
            fragments.push(
                value["delta"]["text"]
                    .as_str()
                    .expect("text fragment")
                    .to_string(),
            );
            wire_lengths.push(line.len());
        }
    }

    assert!(
        fragments.len() > 1,
        "a single large upstream chunk must not become one TUI-blocking delta; lengths={wire_lengths:?}"
    );
    assert!(
        wire_lengths.iter().all(|length| *length <= 2_048),
        "outgoing text delta exceeded render bound: {wire_lengths:?}"
    );
    assert_eq!(fragments.concat(), text);
}
