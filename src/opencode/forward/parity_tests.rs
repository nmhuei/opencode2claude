//! Sync-vs-SSE parity and end-to-end stream lifecycle tests.
//!
//! These tests drive `forward_to_llm_sync` and `forward_to_llm_stream`
//! against a hermetic in-process stub upstream (axum on an ephemeral
//! loopback port) and assert that the same upstream payload produces the
//! same logical Anthropic output on both execution paths — content blocks
//! and terminal stop reason — plus that the streaming lifecycle contract
//! (message_start … message_delta(stop_reason) → message_stop, terminal
//! error ends the stream with nothing after it) holds through the full
//! executor, not just the per-line context helpers.
//!
//! Isolation: egress is Direct (no proxy workers), history capture points at
//! an unwritable path so the store degrades to unavailable and every capture
//! call becomes a no-op; no filesystem or external-network side effects.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::config::{BridgeConfig, EgressMode};
use crate::handlers::{AnthropicTool, ContentVal, Message, MessagesRequest};
use crate::history::{HistoryCapture, HistoryRequestStart};
use crate::opencode::forward::{forward_to_llm_stream, forward_to_llm_sync};
use crate::state::AppState;

const PARITY_KEY: &str = "parity-test-key";
const PARITY_MODEL: &str = "parity-model";

// ── Stub upstream ────────────────────────────────────────────────────────

async fn spawn_stub<F>(handler: F) -> String
where
    F: Fn(Value) -> Response + Clone + Send + 'static,
{
    let app = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post(move |body: axum::body::Bytes| {
            let handler = handler.clone();
            async move {
                let parsed = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
                handler(parsed)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stub binds an ephemeral loopback port");
    let addr = listener.local_addr().expect("stub local addr");
    tokio::spawn(async move {
        // Runs until the test runtime drops it; each stub serves one test.
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn json_response(status: u16, body: Value) -> Response {
    (
        StatusCode::from_u16(status).expect("valid status"),
        axum::Json(body),
    )
        .into_response()
}

fn sse_response(lines: &[String]) -> Response {
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(axum::body::Body::from(text))
        .expect("static SSE body builds")
}

fn wants_stream(request: &Value) -> bool {
    request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn dispatch(script_json: Value, script_sse: Vec<String>) -> impl Fn(Value) -> Response + Clone {
    move |request| {
        if wants_stream(&request) {
            sse_response(&script_sse)
        } else {
            json_response(200, script_json.clone())
        }
    }
}

fn delta_chunk(delta: Value, finish: Option<&str>) -> String {
    format!(
        "data: {}",
        json!({"choices": [{"delta": delta, "finish_reason": finish}]})
    )
}

// ── Bridge state / payload / capture helpers ─────────────────────────────

#[derive(Debug, Default)]
struct NoopContainerRuntime;

#[async_trait::async_trait]
impl crate::docker::ContainerRuntime for NoopContainerRuntime {
    async fn daemon_version(&self) -> crate::docker::DockerResult<String> {
        Ok("test".to_string())
    }
    async fn inspect(
        &self,
        _spec: &crate::docker::ProxySpec,
    ) -> crate::docker::DockerResult<crate::docker::ContainerState> {
        Err(crate::docker::DockerError::CommandFailed(
            "test runtime unavailable".to_string(),
        ))
    }
    async fn create_missing(
        &self,
        _spec: &crate::docker::ProxySpec,
    ) -> crate::docker::DockerResult<()> {
        Ok(())
    }
    async fn recreate_managed(
        &self,
        _spec: &crate::docker::ProxySpec,
    ) -> crate::docker::DockerResult<()> {
        Ok(())
    }
    async fn remove_managed(
        &self,
        _spec: &crate::docker::ProxySpec,
    ) -> crate::docker::DockerResult<()> {
        Ok(())
    }
    async fn restart_managed(
        &self,
        _spec: &crate::docker::ProxySpec,
    ) -> crate::docker::DockerResult<()> {
        Ok(())
    }
    async fn stop_managed(
        &self,
        _spec: &crate::docker::ProxySpec,
    ) -> crate::docker::DockerResult<()> {
        Ok(())
    }
    async fn start_managed(
        &self,
        _spec: &crate::docker::ProxySpec,
    ) -> crate::docker::DockerResult<()> {
        Ok(())
    }
    async fn logs(
        &self,
        _spec: &crate::docker::ProxySpec,
        _tail: usize,
    ) -> crate::docker::DockerResult<String> {
        Ok(String::new())
    }
    async fn list(
        &self,
        _specs: &[crate::docker::ProxySpec],
    ) -> crate::docker::DockerResult<Vec<crate::docker::ContainerSummary>> {
        Ok(Vec::new())
    }
}

fn parity_state(base_url: String) -> AppState {
    parity_state_with_capacity(base_url, BridgeConfig::default().channel_capacity)
}

fn parity_state_with_capacity(base_url: String, channel_capacity: usize) -> AppState {
    let mut config = BridgeConfig::default();
    config.egress.mode = EgressMode::Direct;
    config.retry.upstream_base_url = base_url;
    config.channel_capacity = channel_capacity;
    config.history.enabled = false;
    // Unwritable parent keeps HistoryStore::open from creating anything on
    // disk; begin() then hands out permanently disabled captures.
    config.history.path = Some(std::path::PathBuf::from(
        "/proc/oc2api-parity-guard/history.db",
    ));
    AppState::new_with_container_runtime(config, Arc::new(NoopContainerRuntime))
}

fn disabled_capture(state: &AppState, stream: bool) -> HistoryCapture {
    state.history.begin(HistoryRequestStart {
        id: format!("parity-{}", std::process::id()),
        conversation_id: None,
        parent_request_id: None,
        protocol: "anthropic".to_string(),
        endpoint: "/v1/messages".to_string(),
        operation_kind: "inference".to_string(),
        client_key_id: Some(PARITY_KEY.to_string()),
        client_name: None,
        client_environment: None,
        requested_model: Some(PARITY_MODEL.to_string()),
        effective_model: Some(PARITY_MODEL.to_string()),
        stream,
        thinking_requested: false,
        reasoning_effort: None,
        reasoning_budget_tokens: None,
        inbound: None,
    })
}

fn parity_payload(stream: bool) -> MessagesRequest {
    MessagesRequest {
        model: Some("parity-model".to_string()),
        messages: vec![Message {
            role: "user".to_string(),
            content: ContentVal::Single("do the task".to_string()),
        }],
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            ..Default::default()
        }]),
        stream,
        max_tokens: Some(256),
        ..Default::default()
    }
}

// ── Logical-output normalization ─────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum LogicalBlock {
    Thinking(String),
    Text(String),
    ToolUse { name: String, arguments: Value },
}

fn logical_from_sync(response: &Value) -> (Vec<LogicalBlock>, Option<String>) {
    let mut blocks = Vec::new();
    if let Some(content) = response.get("content").and_then(Value::as_array) {
        for item in content {
            match item.get("type").and_then(Value::as_str) {
                Some("thinking") => blocks.push(LogicalBlock::Thinking(
                    item.get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                )),
                Some("text") => blocks.push(LogicalBlock::Text(
                    item.get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                )),
                Some("tool_use") => blocks.push(LogicalBlock::ToolUse {
                    name: item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    arguments: item.get("input").cloned().unwrap_or(Value::Null),
                }),
                _ => {}
            }
        }
    }
    (
        blocks,
        response
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
    )
}

/// Drain the bridge's event stream into raw SSE wire frames.
///
/// The stream is wrapped in axum's `Sse` response and the body is read back,
/// giving the exact bytes a real client would receive.
async fn collect_stream_events(
    stream: impl futures_util::Stream<Item = Result<axum::response::sse::Event, Infallible>>
        + Send
        + 'static,
) -> Vec<(String, Value)> {
    let response = axum::response::sse::Sse::new(stream).into_response();
    let body = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("SSE body reads");
    let text = String::from_utf8(body.to_vec()).expect("SSE body is UTF-8");

    let mut events = Vec::new();
    for frame in text.split("\n\n") {
        if frame.trim().is_empty() {
            continue;
        }
        let mut name = String::new();
        let mut data = String::new();
        for line in frame.lines() {
            if let Some(rest) = line.strip_prefix("event: ") {
                name = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("data: ") {
                data.push_str(rest);
            }
        }
        let parsed = serde_json::from_str(&data).unwrap_or(Value::Null);
        events.push((name, parsed));
    }
    events
}

fn logical_from_events(events: &[(String, Value)]) -> (Vec<LogicalBlock>, Option<String>) {
    #[derive(Default)]
    struct Accum {
        kind: &'static str,
        text: String,
        name: String,
        arguments: String,
    }

    let mut open: BTreeMap<u64, Accum> = BTreeMap::new();
    let mut stop_reason: Option<String> = None;

    for (name, data) in events {
        match name.as_str() {
            "content_block_start" => {
                let index = data.get("index").and_then(Value::as_u64).unwrap_or(0);
                let block = data.get("content_block").cloned().unwrap_or(Value::Null);
                let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
                open.insert(
                    index,
                    Accum {
                        kind: match kind {
                            "thinking" => "thinking",
                            "tool_use" => "tool_use",
                            _ => "text",
                        },
                        name: block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        ..Default::default()
                    },
                );
            }
            "content_block_delta" => {
                let index = data.get("index").and_then(Value::as_u64).unwrap_or(0);
                let delta = data.get("delta").cloned().unwrap_or(Value::Null);
                let Some(acc) = open.get_mut(&index) else {
                    continue;
                };
                match delta.get("type").and_then(Value::as_str) {
                    Some("thinking_delta") => acc.text.push_str(
                        delta
                            .get("thinking")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    ),
                    Some("text_delta") => acc.text.push_str(
                        delta
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    ),
                    Some("input_json_delta") => acc.arguments.push_str(
                        delta
                            .get("partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    ),
                    _ => {}
                }
            }
            "message_delta" => {
                stop_reason = data
                    .get("delta")
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            _ => {}
        }
    }

    let blocks = open
        .into_values()
        .map(|acc| match acc.kind {
            "thinking" => LogicalBlock::Thinking(acc.text),
            "tool_use" => LogicalBlock::ToolUse {
                name: acc.name,
                arguments: serde_json::from_str(&acc.arguments).unwrap_or(Value::Null),
            },
            _ => LogicalBlock::Text(acc.text),
        })
        .collect();

    (blocks, stop_reason)
}

fn assert_clean_stream_lifecycle(events: &[(String, Value)]) {
    assert!(
        !events.is_empty(),
        "stream must emit at least a lifecycle skeleton"
    );
    assert_eq!(
        events[0].0,
        "message_start",
        "first event must be message_start, got: {:?}",
        events.first().map(|(name, _)| name)
    );
    assert_eq!(events.last().expect("nonempty").0, "message_stop");
    assert_eq!(
        events
            .iter()
            .filter(|(name, _)| name == "message_start")
            .count(),
        1,
        "exactly one message_start expected"
    );
    assert_eq!(
        events
            .iter()
            .filter(|(name, _)| name == "message_delta")
            .count(),
        1,
        "exactly one terminal message_delta expected"
    );
    assert_eq!(
        events
            .iter()
            .filter(|(name, _)| name == "message_stop")
            .count(),
        1,
        "exactly one message_stop expected"
    );
    assert!(
        events.iter().all(|(name, _)| name != "error"),
        "no error event expected on the healthy path"
    );
}

// ── Shared scenario scripts ──────────────────────────────────────────────

fn plain_text_scripts() -> (Value, Vec<String>) {
    (
        json!({
            "id": "chatcmpl-plain", "model": "parity-upstream",
            "choices": [{"message": {"role": "assistant", "content": "Hello parity"},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 2}
        }),
        vec![
            delta_chunk(json!({"content": "Hello "}), None),
            delta_chunk(json!({"content": "parity"}), None),
            delta_chunk(json!({}), Some("stop")),
            "data: [DONE]".to_string(),
        ],
    )
}

fn reasoning_text_scripts() -> (Value, Vec<String>) {
    (
        json!({
            "id": "chatcmpl-reasoning", "model": "parity-upstream",
            "choices": [{"message": {"role": "assistant",
                                     "reasoning_content": "because two plus two",
                                     "content": "equals four"},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 6, "completion_tokens": 4}
        }),
        vec![
            delta_chunk(json!({"reasoning_content": "because two plus two"}), None),
            delta_chunk(json!({"content": "equals four"}), None),
            delta_chunk(json!({}), Some("stop")),
            "data: [DONE]".to_string(),
        ],
    )
}

fn native_tool_scripts() -> (Value, Vec<String>) {
    (
        json!({
            "id": "chatcmpl-tool", "model": "parity-upstream",
            "choices": [{"message": {"role": "assistant", "content": null,
                                     "tool_calls": [{"id": "call_parity_1",
                                                     "function": {"name": "Bash",
                                                                  "arguments": "{\"command\":\"ls\"}"}}]},
                         "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 7}
        }),
        vec![
            delta_chunk(
                json!({"tool_calls": [{"index": 0, "id": "call_parity_1",
                                               "function": {"name": "Bash",
                                                            "arguments": "{\"command\":"}}]}),
                None,
            ),
            delta_chunk(
                json!({"tool_calls": [{"index": 0,
                                               "function": {"arguments": "\"ls\"}"}}]}),
                None,
            ),
            delta_chunk(json!({}), Some("tool_calls")),
            "data: [DONE]".to_string(),
        ],
    )
}

fn compat_marker_scripts() -> (Value, Vec<String>) {
    let marker = "[Requesting Bash with arguments: {\"command\":\"ls\"}]";
    (
        json!({
            "id": "chatcmpl-compat", "model": "parity-upstream",
            "choices": [{"message": {"role": "assistant", "content": marker},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3}
        }),
        vec![
            delta_chunk(json!({"content": marker}), None),
            delta_chunk(json!({}), Some("stop")),
            "data: [DONE]".to_string(),
        ],
    )
}

// ── Parity tests ─────────────────────────────────────────────────────────

macro_rules! parity_case {
    ($test_name:ident, $scripts_fn:ident) => {
        #[tokio::test]
        async fn $test_name() {
            let (script_json, script_sse) = $scripts_fn();

            let url = spawn_stub(dispatch(script_json.clone(), script_sse.clone())).await;
            let state = parity_state(url);
            let sync_response = forward_to_llm_sync(
                &state,
                PARITY_KEY.to_string(),
                parity_payload(false),
                PARITY_MODEL.to_string(),
                state.search_client.clone(),
                state.config.max_search_loops,
                disabled_capture(&state, false),
            )
            .await
            .expect("sync path succeeds against stub");
            let (sync_blocks, sync_stop) = logical_from_sync(&sync_response);

            let url = spawn_stub(dispatch(script_json, script_sse)).await;
            let state = parity_state(url);
            let stream = forward_to_llm_stream(
                &state,
                PARITY_KEY.to_string(),
                parity_payload(true),
                PARITY_MODEL.to_string(),
                state.config.channel_capacity,
                state.search_client.clone(),
                state.config.max_search_loops,
                disabled_capture(&state, true),
            )
            .await
            .expect("stream path starts against stub");
            let events = collect_stream_events(stream).await;
            assert_clean_stream_lifecycle(&events);
            let (stream_blocks, stream_stop) = logical_from_events(&events);

            assert_eq!(
                sync_blocks, stream_blocks,
                "sync and stream paths must produce identical logical content blocks"
            );
            assert_eq!(
                sync_stop, stream_stop,
                "sync and stream paths must agree on the terminal stop reason"
            );
        }
    };
}

parity_case!(
    plain_text_is_logically_identical_on_both_paths,
    plain_text_scripts
);
parity_case!(
    reasoning_then_text_is_logically_identical_on_both_paths,
    reasoning_text_scripts
);
parity_case!(
    native_tool_call_is_logically_identical_on_both_paths,
    native_tool_scripts
);
parity_case!(
    compat_marker_converts_identically_on_both_paths,
    compat_marker_scripts
);

// ── Stream-lifecycle pins (error + pre-emission retry) ───────────────────

#[tokio::test]
async fn midstream_upstream_error_ends_stream_without_message_terminators() {
    let lines = vec![
        delta_chunk(json!({"content": "partial"}), None),
        format!(
            "data: {}",
            json!({"error": {"message": "boom", "type": "server_error"}})
        ),
        "data: [DONE]".to_string(),
    ];
    let url = spawn_stub(move |request| {
        if wants_stream(&request) {
            sse_response(&lines)
        } else {
            unreachable!("error scenario drives only the stream path")
        }
    })
    .await;
    let state = parity_state(url);

    let stream = forward_to_llm_stream(
        &state,
        PARITY_KEY.to_string(),
        parity_payload(true),
        PARITY_MODEL.to_string(),
        state.config.channel_capacity,
        state.search_client.clone(),
        state.config.max_search_loops,
        disabled_capture(&state, true),
    )
    .await
    .expect("stream path starts");

    let events = collect_stream_events(stream).await;
    let last = events.last().expect("at least one event").clone();
    assert_eq!(
        last.0,
        "error",
        "the terminal event must be the api_error event, sequence: {:?}",
        events.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
    assert!(
        events.iter().all(|(name, _)| name != "message_delta"),
        "no message_delta may follow or precede the terminal error"
    );
    assert!(
        events.iter().all(|(name, _)| name != "message_stop"),
        "no message_stop may follow the terminal error"
    );
}

#[tokio::test]
async fn rate_limited_first_attempt_retries_before_any_client_byte() {
    let hits = Arc::new(AtomicU32::new(0));
    let stub_hits = Arc::clone(&hits);
    let success_lines = plain_text_scripts().1;
    let url = spawn_stub(move |request| {
        if stub_hits.fetch_add(1, Ordering::SeqCst) == 0 {
            return json_response(
                429,
                json!({"error": {"message": "rate limited", "type": "requests"}}),
            );
        }
        if wants_stream(&request) {
            sse_response(&success_lines)
        } else {
            unreachable!("rate-limit scenario drives only the stream path")
        }
    })
    .await;
    let state = parity_state(url);

    let stream = forward_to_llm_stream(
        &state,
        PARITY_KEY.to_string(),
        parity_payload(true),
        PARITY_MODEL.to_string(),
        state.config.channel_capacity,
        state.search_client.clone(),
        state.config.max_search_loops,
        disabled_capture(&state, true),
    )
    .await
    .expect("stream path starts");

    let events = collect_stream_events(stream).await;
    assert_clean_stream_lifecycle(&events);
    assert!(
        hits.load(Ordering::SeqCst) >= 2,
        "upstream must have seen the initial 429 followed by a retry"
    );

    let (_, stop) = logical_from_events(&events);
    assert_eq!(stop.as_deref(), Some("end_turn"));
}

// ── Dead-consumer teardown (send-failure semantics) ──────────────────────

#[tokio::test]
async fn stalled_consumer_terminates_stream_without_fake_clean_end() {
    // A consumer that stops polling the SSE channel must not receive a
    // trickle of individually-dropped block events, and the task must stop
    // attempting sends after the first bounded-send failure. Observable,
    // timing-independent contract: the stream ends early with NO terminal
    // message_delta/message_stop (no fake clean end), and the stream is
    // accounted as cancelled — never as completed.
    let mut chunks = Vec::new();
    for i in 0..12 {
        chunks.push(delta_chunk(json!({"content": format!("chunk{i}")}), None));
    }
    chunks.push(delta_chunk(json!({}), Some("stop")));
    chunks.push("data: [DONE]".to_string());
    let offered_events = chunks.len();

    let url = spawn_stub(move |request| {
        if wants_stream(&request) {
            sse_response(&chunks)
        } else {
            unreachable!("dead-consumer scenario drives only the stream path")
        }
    })
    .await;
    // Capacity 2 overflows quickly once the consumer stops polling.
    let state = parity_state_with_capacity(url, 2);

    let stream = forward_to_llm_stream(
        &state,
        PARITY_KEY.to_string(),
        parity_payload(true),
        PARITY_MODEL.to_string(),
        state.config.channel_capacity,
        state.search_client.clone(),
        state.config.max_search_loops,
        disabled_capture(&state, true),
    )
    .await
    .expect("stream path starts");

    // Consume the raw wire bytes like a real client: wrap once in Sse, then
    // read body frames so the stall is expressed by simply not polling.
    let response = axum::response::sse::Sse::new(stream).into_response();
    let mut wire = response.into_body().into_data_stream();
    let mut buffer = String::new();
    let mut names = Vec::new();

    let first_frame = tokio::time::timeout(std::time::Duration::from_secs(5), wire.next())
        .await
        .expect("first frame arrives promptly")
        .expect("first frame reads")
        .expect("first frame body reads");
    push_wire_frames(&first_frame, &mut buffer, &mut names);
    assert_eq!(names.first().map(String::as_str), Some("message_start"));

    // Stall past the 5s bounded send window while the stub body completes
    // and the bridge overflows the capacity-2 channel.
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;

    let drain_started = std::time::Instant::now();
    while let Some(frame) = wire.next().await {
        push_wire_frames(&frame.expect("frame reads"), &mut buffer, &mut names);
    }
    let drain_elapsed = drain_started.elapsed();

    assert!(
        drain_elapsed < std::time::Duration::from_secs(10),
        "channel must close promptly after the first send failure (one bounded window), took {drain_elapsed:?}"
    );
    assert!(
        names.len() < offered_events,
        "stream must terminate early instead of delivering every upstream event, got {} of {offered_events}",
        names.len()
    );
    assert!(
        names.iter().all(|name| name != "message_delta"),
        "terminated stream must not contain a terminal message_delta, got: {names:?}"
    );
    assert!(
        names.iter().all(|name| name != "message_stop"),
        "terminated stream must not fake a clean message_stop, got: {names:?}"
    );

    let snapshot = state.metrics.snapshot();
    assert_eq!(
        snapshot.streams_cancelled, 1,
        "a dead consumer tears the response down as cancelled"
    );
    assert_eq!(
        snapshot.streams_completed, 0,
        "a dead consumer must never be accounted as a completed stream"
    );
}

/// Feed one raw body chunk into the frame splitter, extracting complete
/// `event:` names from every completed `\n\n`-delimited SSE frame.
fn push_wire_frames(chunk: &[u8], buffer: &mut String, names: &mut Vec<String>) {
    buffer.push_str(&String::from_utf8_lossy(chunk));
    while let Some(pos) = buffer.find("\n\n") {
        let frame: String = buffer.drain(..pos + 2).collect();
        if let Some(name) = frame.lines().find_map(|line| {
            line.strip_prefix("event: ")
                .map(|value| value.trim().to_string())
        }) {
            names.push(name);
        }
    }
}

// ── Sync stop-reason safety ──────────────────────────────────────────────

/// An upstream that reports `finish_reason: "tool_calls"` while carrying an
/// empty tool-call batch leaves sync with zero emitted tool_use blocks (the
/// batch classifier passes an empty batch straight through). The response
/// must then carry `stop_reason: "end_turn"` (matching the stream path's
/// semantics), never a blockless `tool_use` stop that would leave Claude
/// Code waiting for tool results that never come.
#[tokio::test]
async fn sync_tool_calls_finish_without_emitted_blocks_never_reports_tool_use() {
    let script_json = json!({
        "id": "chatcmpl-empty-toolbatch", "model": "parity-upstream",
        "choices": [{
            "message": {"role": "assistant"},
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 3}
    });
    let script_sse = vec![
        delta_chunk(json!({}), Some("tool_calls")),
        "data: [DONE]".to_string(),
    ];

    let url = spawn_stub(dispatch(script_json, script_sse)).await;
    let state = parity_state(url);
    let sync_response = forward_to_llm_sync(
        &state,
        PARITY_KEY.to_string(),
        parity_payload(false),
        PARITY_MODEL.to_string(),
        state.search_client.clone(),
        state.config.max_search_loops,
        disabled_capture(&state, false),
    )
    .await
    .expect("sync path succeeds against stub");

    assert_eq!(
        sync_response.get("stop_reason").and_then(Value::as_str),
        Some("end_turn"),
        "a filtered-out tool batch must fall back to end_turn, got: {sync_response}"
    );
    let has_tool_use = sync_response
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        });
    assert!(
        !has_tool_use,
        "no tool_use block may accompany a filtered-out batch: {sync_response}"
    );
}
