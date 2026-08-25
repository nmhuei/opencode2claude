//! Local handling for Claude Code's session-title request.
//!
//! Claude Code sends a separate model call to name each session. The request is
//! deterministic UI metadata, so forwarding it to the constrained free upstream
//! needlessly consumes provider quota. Keep detection deliberately narrow so
//! ordinary structured-output requests are never intercepted.

use super::{ContentVal, MessagesRequest};
use crate::sse::SseEventBuilder;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::StreamExt;
use serde_json::Value;

const TITLE_PROMPT_MARKER: &str =
    "Generate a concise, sentence-case title (3-7 words) that captures the main topic or goal of this coding session.";
const TITLE_JSON_MARKER: &str = "Return JSON with a single \"title\" field.";
const CLAUDE_SDK_MARKER: &str = "cc_entrypoint=sdk-cli";

pub(super) fn try_handle(payload: &MessagesRequest, model: String) -> Option<Response> {
    if !is_claude_code_title_request(payload) {
        return None;
    }

    let title = synthesize_title(payload).unwrap_or_else(|| "Coding session".to_string());
    let text = serde_json::json!({"title": title}).to_string();
    let input_tokens = 1;
    let output_tokens = ((text.len() as u32).saturating_add(3) / 4).max(1);
    let builder = SseEventBuilder::new("msg_local_session_title".to_string(), model);

    if !payload.stream {
        return Some(
            Json(builder.non_streaming_response(&text, input_tokens, output_tokens))
                .into_response(),
        );
    }

    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tokio::spawn(async move {
        for event in [
            builder.message_start(input_tokens),
            builder.content_block_start(),
            builder.text_delta(&text),
            builder.content_block_stop(),
            builder.message_delta(output_tokens),
            builder.message_stop(),
        ] {
            if tx.send(event).await.is_err() {
                break;
            }
        }
    });

    Some(sse_response(rx))
}

pub(super) fn is_claude_code_title_request(payload: &MessagesRequest) -> bool {
    if payload
        .tools
        .as_ref()
        .is_some_and(|tools| !tools.is_empty())
        || payload.tool_choice.is_some()
        || payload.thinking_enabled() != Some(false)
        || payload.messages.len() != 1
        || payload.messages[0].role != "user"
    {
        return false;
    }

    let system = system_text(payload.system.as_ref());
    if !system.contains(CLAUDE_SDK_MARKER)
        || !system.contains(TITLE_PROMPT_MARKER)
        || !system.contains(TITLE_JSON_MARKER)
    {
        return false;
    }

    let Some(format) = payload
        .output_config
        .as_ref()
        .and_then(|output| output.format.as_ref())
    else {
        return false;
    };
    if !is_title_only_schema(format) {
        return false;
    }

    session_text(payload).is_some()
}

pub(super) fn synthesize_title(payload: &MessagesRequest) -> Option<String> {
    let session = session_text(payload)?;
    let normalized = session.split_whitespace().collect::<Vec<_>>();
    if normalized.is_empty() {
        return None;
    }

    let mut title = normalized.into_iter().take(5).collect::<Vec<_>>().join(" ");
    title = title
        .trim_matches(|c: char| c.is_ascii_punctuation() && c != '-' && c != '_')
        .to_string();
    (!title.is_empty()).then_some(title)
}

fn is_title_only_schema(format: &Value) -> bool {
    if format.get("type").and_then(Value::as_str) != Some("json_schema") {
        return false;
    }
    let Some(schema) = format.get("schema").and_then(Value::as_object) else {
        return false;
    };
    if schema.get("type").and_then(Value::as_str) != Some("object")
        || schema.get("additionalProperties").and_then(Value::as_bool) != Some(false)
    {
        return false;
    }
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return false;
    };
    if properties.len() != 1
        || properties
            .get("title")
            .and_then(Value::as_object)
            .and_then(|title| title.get("type"))
            .and_then(Value::as_str)
            != Some("string")
    {
        return false;
    }
    let Some(required) = schema.get("required").and_then(Value::as_array) else {
        return false;
    };
    required.len() == 1 && required[0].as_str() == Some("title")
}

fn system_text(system: Option<&Value>) -> String {
    match system {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn session_text(payload: &MessagesRequest) -> Option<String> {
    let text = match &payload.messages.first()?.content {
        ContentVal::Single(text) => text.clone(),
        ContentVal::Multiple(parts) => parts
            .iter()
            .filter(|part| part.content_type == "text")
            .filter_map(|part| part.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n"),
    };
    let start = text.find("<session>")? + "<session>".len();
    let end = text[start..].find("</session>")? + start;
    Some(text[start..end].trim().to_string())
}

fn sse_response(rx: tokio::sync::mpsc::Receiver<Event>) -> Response {
    let response = Sse::new(
        tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok::<_, std::convert::Infallible>),
    )
    .keep_alive(KeepAlive::default())
    .into_response();
    super::messages::disable_proxy_buffering(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured_title_request() -> MessagesRequest {
        serde_json::from_value(serde_json::json!({
            "model": "claude-opus-5",
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": "<session>\nFix the retry state machine for Claude Code\n</session>\n\nWrite the title in the predominant language of the session — a stray word or code token in another language doesn't change it. Ignore the language of the examples above."
                }]
            }],
            "stream": true,
            "max_tokens": 128000,
            "thinking": {"type": "disabled"},
            "output_config": {
                "effort": "high",
                "format": {
                    "type": "json_schema",
                    "schema": {
                        "additionalProperties": false,
                        "properties": {"title": {"type": "string"}},
                        "required": ["title"],
                        "type": "object"
                    }
                }
            },
            "system": [
                {"type":"text","text":"x-anthropic-billing-header: cc_version=2.1.229.143; cc_entrypoint=sdk-cli;"},
                {"type":"text","text":"You are a Claude agent, built on Anthropic's Claude Agent SDK."},
                {"type":"text","text":"Generate a concise, sentence-case title (3-7 words) that captures the main topic or goal of this coding session. Return JSON with a single \"title\" field."}
            ],
            "tools": []
        })).expect("captured request parses")
    }

    #[test]
    fn recognizes_exact_claude_code_title_request_shape() {
        let request = captured_title_request();
        assert!(is_claude_code_title_request(&request));
        assert_eq!(
            synthesize_title(&request).as_deref(),
            Some("Fix the retry state machine")
        );
    }

    #[test]
    fn rejects_near_miss_structured_output_request() {
        let mut request = captured_title_request();
        request.system = Some(serde_json::json!([{
            "type":"text",
            "text":"Return JSON with a single title field for this API response."
        }]));
        assert!(!is_claude_code_title_request(&request));
    }

    #[test]
    fn rejects_title_schema_with_extra_property() {
        let mut request = captured_title_request();
        request.output_config.as_mut().unwrap().format = Some(serde_json::json!({
            "type":"json_schema",
            "schema": {
                "type":"object",
                "additionalProperties":false,
                "properties":{"title":{"type":"string"},"slug":{"type":"string"}},
                "required":["title"]
            }
        }));
        assert!(!is_claude_code_title_request(&request));
    }
}
