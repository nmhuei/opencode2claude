use super::context::{
    finalize_stream_with_text, process_openai_sse_line, push_bounded, split_pending_text,
    StreamContext, MAX_ACCUMULATOR_BYTES, MAX_COMPAT_TOOL_BUFFER_SIZE, MAX_NATIVE_PENDING_BYTES,
    MAX_NATIVE_TOOL_ARGUMENT_BYTES,
};
use crate::handlers::{AnthropicTool, ContentVal, Message, MessagesRequest};
use crate::opencode::forward::common::{
    extract_compat_tool_requests, extract_compat_tool_requests_detailed, get_correct_tool_name,
    parse_compat_tool_request, parse_compat_tool_request_at_eof,
};
use crate::opencode::sanitize::{extract_and_clean_dsml_detailed, strip_system_tags};
use crate::sse::SseEventBuilder;
use crate::stream_tracker::SseBlockTracker;
use axum::body::to_bytes;
use axum::response::{sse::Sse, IntoResponse};
use futures_util::stream;
use std::convert::Infallible;

mod bounds;
mod chunk_parsing;
mod compat;
mod context;
mod direct_markers;
mod dsml;
mod native;
mod search;
mod xml_toolcalls;

fn empty_messages_request() -> MessagesRequest {
    MessagesRequest {
        model: Some("model".to_string()),
        messages: vec![],
        system: None,
        tools: None,
        tool_choice: None,
        stream: true,
        temperature: None,
        max_tokens: Some(100),
        ..Default::default()
    }
}

fn payload_with_tools(names: &[&str]) -> MessagesRequest {
    MessagesRequest {
        tools: Some(
            names
                .iter()
                .map(|name| AnthropicTool {
                    name: (*name).to_string(),
                    description: format!("{name} tool"),
                    input_schema: serde_json::json!({"type":"object"}),
                    ..Default::default()
                })
                .collect(),
        ),
        ..empty_messages_request()
    }
}

async fn serialize_sse_events(events: Vec<axum::response::sse::Event>) -> String {
    let response = Sse::new(stream::iter(
        events
            .into_iter()
            .map(Ok::<axum::response::sse::Event, Infallible>),
    ))
    .into_response();
    String::from_utf8(
        to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .expect("serialize SSE body")
            .to_vec(),
    )
    .expect("SSE body must be UTF-8")
}

#[allow(dead_code)]
fn bash_tool_payload() -> MessagesRequest {
    MessagesRequest {
        tools: Some(vec![AnthropicTool {
            name: "Bash".to_string(),
            description: "execute a shell command".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
            ..Default::default()
        }]),
        ..empty_messages_request()
    }
}

#[allow(dead_code)]
fn feed_reasoning(line_text: &str) -> String {
    format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"reasoning_content": line_text}, "finish_reason": null}]
        })
    )
}

#[allow(dead_code)]
fn feed_text(line_text: &str) -> String {
    format!(
        "data: {}",
        serde_json::json!({
            "choices": [{"delta": {"content": line_text}, "finish_reason": null}]
        })
    )
}
