//! Dashboard event stream, heartbeat, and synthetic stream diagnostics.

use super::auth::check_admin_token;
use super::time::unix_timestamp;
use crate::state::AppState;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use futures_util::Stream;
use serde::Serialize;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{info, warn};

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
    check_admin_token(&headers, token)?;

    let mut rx = state.event_tx.subscribe();
    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
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
                    let fallback = DashboardEvent::DashboardError {
                        message: format!("dropped {} events", n),
                        timestamp: unix_timestamp(),
                    };
                    let json = serde_json::to_string(&fallback).unwrap_or_default();
                    yield Ok(Event::default().data(json));
                }
                Err(broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    };

    let sse = Sse::new(stream)
        .keep_alive(KeepAlive::default().interval(Duration::from_secs(SSE_KEEPALIVE_SECS)));
    Ok(sse)
}

/// GET /api/dashboard/test/stream — synthetic Anthropic SSE stream for UI testing.
pub async fn handler_test_stream_get(
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<
    Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>,
    (StatusCode, Json<Value>),
> {
    let token = params.get("token").map(|s| s.as_str());
    check_admin_token(&headers, token)?;

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
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> Result<
    Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>>,
    (StatusCode, Json<Value>),
> {
    let token = params.get("token").map(|s| s.as_str());
    check_admin_token(&headers, token)?;

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
    let stream = async_stream::stream! {
        let sleep = Duration::from_millis(delay_ms);
        yield Ok(Event::default()
            .event("message_start")
            .json_data(json!({
                "type": "message_start",
                "message": {
                    "id": "msg_dashboard_test_stream",
                    "type": "message",
                    "role": "assistant",
                    "model": "dashboard/synthetic-stream",
                    "content": [],
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 }
                }
            }))
            .unwrap_or_else(|_| Event::default().data("{}")));
        tokio::time::sleep(sleep).await;

        yield Ok(Event::default()
            .event("content_block_start")
            .json_data(json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "thinking", "thinking": "" }
            }))
            .unwrap_or_else(|_| Event::default().data("{}")));

        for chunk in chunk_text_for_stream(&thinking) {
            tokio::time::sleep(sleep).await;
            yield Ok(Event::default()
                .event("content_block_delta")
                .json_data(json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": { "type": "thinking_delta", "thinking": chunk }
                }))
                .unwrap_or_else(|_| Event::default().data("{}")));
        }

        tokio::time::sleep(sleep).await;
        yield Ok(Event::default()
            .event("content_block_stop")
            .json_data(json!({ "type": "content_block_stop", "index": 0 }))
            .unwrap_or_else(|_| Event::default().data("{}")));

        tokio::time::sleep(sleep).await;
        yield Ok(Event::default()
            .event("content_block_start")
            .json_data(json!({
                "type": "content_block_start",
                "index": 1,
                "content_block": { "type": "text", "text": "" }
            }))
            .unwrap_or_else(|_| Event::default().data("{}")));

        for chunk in chunk_text_for_stream(&text) {
            tokio::time::sleep(sleep).await;
            yield Ok(Event::default()
                .event("content_block_delta")
                .json_data(json!({
                    "type": "content_block_delta",
                    "index": 1,
                    "delta": { "type": "text_delta", "text": chunk }
                }))
                .unwrap_or_else(|_| Event::default().data("{}")));
        }

        tokio::time::sleep(sleep).await;
        yield Ok(Event::default()
            .event("content_block_stop")
            .json_data(json!({ "type": "content_block_stop", "index": 1 }))
            .unwrap_or_else(|_| Event::default().data("{}")));

        tokio::time::sleep(sleep).await;
        let output_tokens = ((thinking.len() + text.len()) / 4).max(1);
        yield Ok(Event::default()
            .event("message_delta")
            .json_data(json!({
                "type": "message_delta",
                "delta": { "stop_reason": "end_turn", "stop_sequence": null },
                "usage": { "output_tokens": output_tokens }
            }))
            .unwrap_or_else(|_| Event::default().data("{}")));

        yield Ok(Event::default()
            .event("message_stop")
            .json_data(json!({ "type": "message_stop" }))
            .unwrap_or_else(|_| Event::default().data("{}")));
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default().interval(Duration::from_secs(SSE_KEEPALIVE_SECS)))
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

/// Spawn a heartbeat task that sends a `Heartbeat` event every 30 seconds.
pub fn spawn_heartbeat(event_tx: broadcast::Sender<DashboardEvent>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
        loop {
            interval.tick().await;
            if event_tx
                .send(DashboardEvent::Heartbeat {
                    timestamp: unix_timestamp(),
                })
                .is_err()
            {
                // No subscribers — that's fine, keep going
            }
        }
    });
    info!(
        "Dashboard heartbeat task spawned ({}s interval).",
        HEARTBEAT_INTERVAL_SECS
    );
}
