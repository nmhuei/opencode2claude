//! Dashboard event stream, heartbeat, and synthetic stream diagnostics.

use super::auth::check_admin_token;
use super::time::unix_timestamp;
use crate::sse::SseEventBuilder;
use crate::state::AppState;
use crate::workers::WorkerContext;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use futures_util::Stream;
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::warn;

const SSE_KEEPALIVE_SECS: u64 = 15;
const HEARTBEAT_INTERVAL_SECS: u64 = 30;

/// Events emitted to dashboard SSE clients.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum DashboardEvent {
    /// Proxy status changed (started, stopped, restarted, cooldown, recovered).
    #[serde(rename = "proxy_status")]
    ProxyStatus {
        port: u16,
        status: String,
        timestamp: String,
    },
    /// Log message from a proxy container.
    #[serde(rename = "proxy_log")]
    ProxyLog {
        port: u16,
        message: String,
        level: String,
        timestamp: String,
    },
    /// Configuration was saved.
    #[serde(rename = "config_saved")]
    ConfigSaved { timestamp: String },
    /// Periodic heartbeat to keep SSE connections alive.
    #[serde(rename = "heartbeat")]
    Heartbeat { timestamp: String },
    /// Error event for the dashboard.
    #[serde(rename = "error")]
    DashboardError { message: String, timestamp: String },
}

/// GET /api/dashboard/events — SSE event stream for real-time dashboard updates.
pub async fn handler_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<
    Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>,
    (StatusCode, Json<Value>),
> {
    let token = params.get("token").map(|s| s.as_str());
    check_admin_token(&state, &headers, token)?;

    let mut rx = state.event_tx.subscribe();
    let cancellation = state.workers.cancellation_token();
    let stream = async_stream::stream! {
        loop {
            tokio::select! {
                _ = cancellation.cancelled() => break,
                received = rx.recv() => match received {
                    Ok(event) => {
                        let event_type = match &event {
                            DashboardEvent::ProxyStatus { .. } => "proxy_status",
                            DashboardEvent::ProxyLog { .. } => "proxy_log",
                            DashboardEvent::ConfigSaved { .. } => "config_saved",
                            DashboardEvent::Heartbeat { .. } => "heartbeat",
                            DashboardEvent::DashboardError { .. } => "error",
                        };
                        let json = serde_json::to_string(&event).unwrap_or_default();
                        yield Ok(Event::default().event(event_type).data(json));
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Dashboard SSE client lagged, dropped {} events", n);
                        yield Ok(lagged_fallback_frame(n));
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    };

    let sse = Sse::new(stream)
        .keep_alive(KeepAlive::default().interval(Duration::from_secs(SSE_KEEPALIVE_SECS)));
    Ok(sse)
}

/// Render the lagged-consumer fallback frame for a client that fell behind
/// the broadcast bus.
///
/// Must be a NAMED `error` event: dashboard clients subscribe strictly by SSE
/// event name, so a bare data frame — even one whose JSON body says
/// `"type":"error"` — never reaches the operator. The payload bytes are the
/// `DashboardEvent::DashboardError` serialization, unchanged from the previous
/// bare-frame emission; only the framing gains the name.
fn lagged_fallback_frame(dropped: u64) -> Event {
    let fallback = DashboardEvent::DashboardError {
        message: format!("dropped {} events", dropped),
        timestamp: unix_timestamp(),
    };
    let json = serde_json::to_string(&fallback).unwrap_or_default();
    SseEventBuilder::new(
        "msg_dashboard_event_bus".to_string(),
        "dashboard/event-bus".to_string(),
    )
    .named_event("error", json)
}

/// GET /api/dashboard/test/stream — synthetic Anthropic SSE stream for UI testing.
pub async fn handler_test_stream_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<
    Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>,
    (StatusCode, Json<Value>),
> {
    let token = params.get("token").map(|s| s.as_str());
    check_admin_token(&state, &headers, token)?;

    let thinking = params
        .get("thinking")
        .cloned()
        .unwrap_or_else(default_test_thinking);
    let text = params
        .get("text")
        .cloned()
        .unwrap_or_else(default_test_text);
    let delay_ms = parse_test_delay(params.get("delay_ms"));

    Ok(synthetic_test_stream(thinking, text, delay_ms))
}

/// POST /api/dashboard/test/stream — synthetic Anthropic SSE stream for UI testing.
pub async fn handler_test_stream_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> Result<
    Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>,
    (StatusCode, Json<Value>),
> {
    let token = params.get("token").map(|s| s.as_str());
    check_admin_token(&state, &headers, token)?;

    let body_value = body.as_ref().map(|b| b.0.clone());
    let thinking = body_value
        .as_ref()
        .and_then(|v| v.get("thinking"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| params.get("thinking").cloned())
        .unwrap_or_else(default_test_thinking);
    let text = body_value
        .as_ref()
        .and_then(|v| v.get("text"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| params.get("text").cloned())
        .unwrap_or_else(default_test_text);
    let delay_ms = body_value
        .as_ref()
        .and_then(|v| v.get("delay_ms"))
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| parse_test_delay(params.get("delay_ms")));

    Ok(synthetic_test_stream(thinking, text, delay_ms))
}

fn default_test_thinking() -> String {
    "I will add the three request counts step by step: 12 + 18 = 30, then 30 + 25 = 55.".to_string()
}

fn default_test_text() -> String {
    "The total number of requests is 55.".to_string()
}

fn parse_test_delay(value: Option<&String>) -> u64 {
    value
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(40)
        .clamp(0, 1000)
}

fn synthetic_test_stream(
    thinking: String,
    text: String,
    delay_ms: u64,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    Sse::new(synthetic_event_stream(thinking, text, delay_ms))
        .keep_alive(KeepAlive::default().interval(Duration::from_secs(SSE_KEEPALIVE_SECS)))
}

fn synthetic_event_stream(
    thinking: String,
    text: String,
    delay_ms: u64,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    // Shared Anthropic-SSE factory (crate::sse) instead of hand-rolled json!
    // frames; byte-parity with the previous emitters is pinned by the
    // characterization tests below.
    let builder = SseEventBuilder::new(
        "msg_dashboard_test_stream".to_string(),
        "dashboard/synthetic-stream".to_string(),
    );
    async_stream::stream! {
        let sleep = Duration::from_millis(delay_ms);
        yield Ok(builder.message_start(0));
        tokio::time::sleep(sleep).await;

        yield Ok(builder.content_block_start_at(0, "thinking", None, None));

        for chunk in chunk_text_for_stream(&thinking) {
            tokio::time::sleep(sleep).await;
            yield Ok(builder.thinking_delta(0, &chunk));
        }

        tokio::time::sleep(sleep).await;
        yield Ok(builder.content_block_stop_at(0));

        tokio::time::sleep(sleep).await;
        yield Ok(builder.content_block_start_at(1, "text", None, None));

        for chunk in chunk_text_for_stream(&text) {
            tokio::time::sleep(sleep).await;
            yield Ok(builder.text_delta_at(1, &chunk));
        }

        tokio::time::sleep(sleep).await;
        yield Ok(builder.content_block_stop_at(1));

        tokio::time::sleep(sleep).await;
        let output_tokens = ((thinking.len() + text.len()) / 4).max(1) as u32;
        yield Ok(builder.message_delta(output_tokens));

        yield Ok(builder.message_stop());
    }
}

fn chunk_text_for_stream(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    for ch in text.chars() {
        current.push(ch);
        current_len += ch.len_utf8();
        if current_len >= 8 || matches!(ch, ' ' | '.' | ',' | ':' | ';' | '\n') {
            chunks.push(std::mem::take(&mut current));
            current_len = 0;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Run the dashboard heartbeat until application cancellation.
pub async fn run_heartbeat(
    event_tx: broadcast::Sender<DashboardEvent>,
    context: WorkerContext,
) -> Result<(), String> {
    let mut interval = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
    loop {
        tokio::select! {
            _ = context.cancellation().cancelled() => return Ok(()),
            _ = interval.tick() => {
                context.heartbeat();
                let _ = event_tx.send(DashboardEvent::Heartbeat {
                    timestamp: unix_timestamp(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render an event's wire buffer the same way `crate::sse` tests do:
    /// Debug formatting with backslash-escapes stripped, so `\n` between SSE
    /// fields reads as a bare `n` and JSON quotes read literally.
    fn plain(event: &Event) -> String {
        format!("{event:?}").replace('\\', "")
    }

    async fn collect_default_stream() -> Vec<Event> {
        use futures_util::StreamExt;
        synthetic_event_stream(default_test_thinking(), default_test_text(), 0)
            .map(|result| result.unwrap())
            .collect()
            .await
    }

    // Golden wire bytes captured from the pre-migration hand-rolled emitters
    // (serde_json Map is a BTreeMap, so object keys serialize alphabetically;
    // the stray `n` characters are backslash-stripped `\n` field separators).
    const GOLDEN_MESSAGE_START: &str = concat!(
        r#"Event { buffer: b"event: message_startndata: {"#,
        r#""message":{"content":[],"id":"msg_dashboard_test_stream","#,
        r#""model":"dashboard/synthetic-stream","role":"assistant","#,
        r#""stop_reason":null,"stop_sequence":null,"type":"message","#,
        r#""usage":{"input_tokens":0,"output_tokens":0}},"type":"message_start"}n""#,
        r#", flags: EventFlags(3) }"#
    );
    const GOLDEN_THINKING_START: &str = concat!(
        r#"Event { buffer: b"event: content_block_startndata: {"#,
        r#""content_block":{"thinking":"","type":"thinking"},"index":0,"#,
        r#""type":"content_block_start"}n", flags: EventFlags(3) }"#
    );
    const GOLDEN_THINKING_STOP: &str = concat!(
        r#"Event { buffer: b"event: content_block_stopndata: "#,
        r#"{"index":0,"type":"content_block_stop"}n", flags: EventFlags(3) }"#
    );
    const GOLDEN_TEXT_START: &str = concat!(
        r#"Event { buffer: b"event: content_block_startndata: "#,
        r#"{"content_block":{"text":"","type":"text"},"index":1,"type":"content_block_start"}n""#,
        r#", flags: EventFlags(3) }"#
    );
    const GOLDEN_TEXT_STOP: &str = concat!(
        r#"Event { buffer: b"event: content_block_stopndata: "#,
        r#"{"index":1,"type":"content_block_stop"}n", flags: EventFlags(3) }"#
    );
    const GOLDEN_MESSAGE_DELTA: &str = concat!(
        r#"Event { buffer: b"event: message_deltandata: {"delta":{"stop_reason":"end_turn","#,
        r#""stop_sequence":null},"type":"message_delta","usage":{"output_tokens":29}}n""#,
        r#", flags: EventFlags(3) }"#
    );
    const GOLDEN_MESSAGE_STOP: &str = concat!(
        r#"Event { buffer: b"event: message_stopndata: {"type":"message_stop"}n", "#,
        r#"flags: EventFlags(3) }"#
    );

    /// Exact delta-chunk sequences produced by `chunk_text_for_stream` for the
    /// default thinking/text payloads (captured alongside the golden frames).
    const GOLDEN_THINKING_CHUNKS: [&str; 23] = [
        "I ", "will ", "add ", "the ", "three ", "request ", "counts ", "step ", "by ", "step:",
        " ", "12 ", "+ ", "18 ", "= ", "30,", " ", "then ", "30 ", "+ ", "25 ", "= ", "55.",
    ];
    const GOLDEN_TEXT_CHUNKS: [&str; 8] = [
        "The ", "total ", "number ", "of ", "requests", " ", "is ", "55.",
    ];

    #[test]
    fn default_payloads_match_the_golden_chunk_arithmetic() {
        // The golden message_delta pins output_tokens = 29; recompute the
        // production formula `((thinking + text) / 4).max(1)`'s inputs so the
        // constant cannot silently rot. (The .max(1) floor is unreachable for
        // these non-empty defaults.)
        let total = default_test_thinking().len() + default_test_text().len();
        assert_eq!(total, 117);
        assert_eq!(total / 4, 29);
        assert_eq!(
            chunk_text_for_stream(&default_test_thinking()),
            GOLDEN_THINKING_CHUNKS
        );
        assert_eq!(
            chunk_text_for_stream(&default_test_text()),
            GOLDEN_TEXT_CHUNKS
        );
    }

    /// Byte-for-byte characterization of the whole synthetic Anthropic
    /// lifecycle: every frame must render exactly the bytes the hand-rolled
    /// `json!` emitters produced before the SseEventBuilder migration.
    #[tokio::test]
    async fn synthetic_stream_frames_match_pre_migration_golden_bytes() {
        let events = collect_default_stream().await;
        assert_eq!(events.len(), 38);

        assert_eq!(plain(&events[0]), GOLDEN_MESSAGE_START);
        assert_eq!(plain(&events[1]), GOLDEN_THINKING_START);

        let mut cursor = 2;
        for chunk in GOLDEN_THINKING_CHUNKS {
            let expected = format!(
                r#"Event {{ buffer: b"event: content_block_deltandata: {{"delta":{{"thinking":"{chunk}","type":"thinking_delta"}},"index":0,"type":"content_block_delta"}}n", flags: EventFlags(3) }}"#
            );
            assert_eq!(plain(&events[cursor]), expected, "thinking chunk {chunk:?}");
            cursor += 1;
        }
        assert_eq!(cursor, 25);
        assert_eq!(plain(&events[25]), GOLDEN_THINKING_STOP);

        assert_eq!(plain(&events[26]), GOLDEN_TEXT_START);
        cursor = 27;
        for chunk in GOLDEN_TEXT_CHUNKS {
            let expected = format!(
                r#"Event {{ buffer: b"event: content_block_deltandata: {{"delta":{{"text":"{chunk}","type":"text_delta"}},"index":1,"type":"content_block_delta"}}n", flags: EventFlags(3) }}"#
            );
            assert_eq!(plain(&events[cursor]), expected, "text chunk {chunk:?}");
            cursor += 1;
        }
        assert_eq!(cursor, 35);
        assert_eq!(plain(&events[35]), GOLDEN_TEXT_STOP);
        assert_eq!(plain(&events[36]), GOLDEN_MESSAGE_DELTA);
        assert_eq!(plain(&events[37]), GOLDEN_MESSAGE_STOP);
    }

    /// Structural contract of the synthetic stream: canonical lifecycle order,
    /// strictly increasing block indices, terminal message_stop last.
    #[tokio::test]
    async fn synthetic_stream_lifecycle_order_and_terminality() {
        let events = collect_default_stream().await;
        let names: Vec<String> = events
            .iter()
            .map(|e| {
                let rendered = plain(e);
                rendered
                    .split("ndata:")
                    .next()
                    .unwrap_or_default()
                    .trim_start_matches(r#"Event { buffer: b"event: "#)
                    .to_string()
            })
            .collect();

        let expected: Vec<&str> = std::iter::once("message_start")
            .chain(std::iter::once("content_block_start"))
            .chain(std::iter::repeat_n(
                "content_block_delta",
                GOLDEN_THINKING_CHUNKS.len(),
            ))
            .chain(["content_block_stop", "content_block_start"])
            .chain(std::iter::repeat_n(
                "content_block_delta",
                GOLDEN_TEXT_CHUNKS.len(),
            ))
            .chain(["content_block_stop", "message_delta", "message_stop"])
            .collect();
        assert_eq!(names, expected);
        assert_eq!(
            *names.last().unwrap(),
            "message_stop",
            "stream must end terminally"
        );

        // Block indices: thinking block is index 0, text block index 1, and
        // stop_reason appears only in the single message_delta frame.
        let joined = events.iter().map(plain).collect::<Vec<_>>().join("\u{1}");
        assert!(joined.contains(r#"event: content_block_stopndata: {"index":0"#));
        assert!(joined.contains(r#"event: content_block_stopndata: {"index":1"#));
        // stop_reason appears twice by design: null inside message_start's
        // message envelope, "end_turn" in the single terminal message_delta.
        assert_eq!(joined.matches("\"stop_reason\":\"end_turn\"").count(), 1);
        assert_eq!(joined.matches("\"stop_reason\":null").count(), 1);
    }

    /// Regression: the lagged-consumer fallback used to emit a BARE data
    /// frame — the `type: error` lived only inside the JSON body, and the
    /// dashboard UI subscribes strictly by SSE event name, so "dropped N
    /// events" notifications never reached the operator during event-bus
    /// overload. The fallback must carry the same named `error` framing the
    /// Ok-path uses for `DashboardEvent::DashboardError`, with an unchanged
    /// payload (internally-tagged serde enum: `type` tag first, then fields
    /// in declaration order).
    #[test]
    fn lagged_fallback_frame_is_a_named_error_event() {
        let rendered = plain(&lagged_fallback_frame(3));
        let buffer = rendered
            .strip_prefix(r#"Event { buffer: b""#)
            .unwrap_or_else(|| panic!("unexpected Event debug shape: {rendered}"));
        assert!(
            buffer.starts_with(
                r#"event: errorndata: {"type":"error","message":"dropped 3 events","timestamp":""#
            ),
            "lagged fallback must be a named `error` frame with type-tag-first payload, got: {rendered}"
        );
        assert!(
            buffer.ends_with(r#"}n", flags: EventFlags(3) }"#),
            "payload must stay a single closed JSON object, got: {rendered}"
        );
    }

    /// Path A (dashboard event bus) keeps its own wire contract: the enum's
    /// serde tagging feeds `.data(...)` verbatim. Internally-tagged enums
    /// emit the `type` tag FIRST, then fields in declaration order (not
    /// alphabetical like `json!` maps). Timestamps vary, so payloads are
    /// pinned modulo the timestamp value.
    #[test]
    fn dashboard_event_payloads_serialize_with_type_tag_first_shape() {
        let heartbeat = serde_json::to_string(&DashboardEvent::Heartbeat {
            timestamp: "TS".to_string(),
        })
        .unwrap();
        assert_eq!(heartbeat, r#"{"type":"heartbeat","timestamp":"TS"}"#);

        let status = serde_json::to_string(&DashboardEvent::ProxyStatus {
            port: 40001,
            status: "started".to_string(),
            timestamp: "TS".to_string(),
        })
        .unwrap();
        assert_eq!(
            status,
            r#"{"type":"proxy_status","port":40001,"status":"started","timestamp":"TS"}"#
        );

        let log = serde_json::to_string(&DashboardEvent::ProxyLog {
            port: 40002,
            message: "m".to_string(),
            level: "info".to_string(),
            timestamp: "TS".to_string(),
        })
        .unwrap();
        assert_eq!(
            log,
            r#"{"type":"proxy_log","port":40002,"message":"m","level":"info","timestamp":"TS"}"#
        );

        let saved = serde_json::to_string(&DashboardEvent::ConfigSaved {
            timestamp: "TS".to_string(),
        })
        .unwrap();
        assert_eq!(saved, r#"{"type":"config_saved","timestamp":"TS"}"#);

        let error = serde_json::to_string(&DashboardEvent::DashboardError {
            message: "dropped 3 events".to_string(),
            timestamp: "TS".to_string(),
        })
        .unwrap();
        assert_eq!(
            error,
            r#"{"type":"error","message":"dropped 3 events","timestamp":"TS"}"#
        );
    }
}
