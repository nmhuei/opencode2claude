//! SSE (Server-Sent Events) builder for Anthropic-compatible streaming responses.
//!
//! This module eliminates code duplication between shell and OpenCode streaming
//! by providing a unified event builder that constructs properly formatted
//! Anthropic SSE events.

use axum::response::sse::Event;
use serde_json::json;

/// Builder for constructing Anthropic-compatible SSE events.
///
/// Encapsulates the message ID and model name, providing methods
/// to generate each event type in the streaming protocol.
#[derive(Debug, Clone)]
pub struct SseEventBuilder {
    msg_id: String,
    model: String,
}

impl SseEventBuilder {
    /// Create a new builder with the given message ID and model name.
    pub fn new(msg_id: String, model: String) -> Self {
        Self { msg_id, model }
    }

    /// Generate the `message_start` event — sent at the beginning of a response.
    pub fn message_start(&self, input_tokens: u32) -> Event {
        Event::default()
            .event("message_start")
            .json_data(json!({
                "type": "message_start",
                "message": {
                    "id": self.msg_id,
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": self.model,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {"input_tokens": input_tokens, "output_tokens": 0}
                }
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Generate a `content_block_start` event at the given index.
    ///
    /// `block_type` is one of `"text"`, `"thinking"`, `"tool_use"`.
    /// `id` and `name` are only used for `tool_use` blocks.
    pub fn content_block_start(
        &self,
        index: usize,
        block_type: &str,
        id: Option<&str>,
        name: Option<&str>,
    ) -> Event {
        let mut block = serde_json::Map::new();
        block.insert("type".to_string(), json!(block_type));
        if block_type == "text" {
            block.insert("text".to_string(), json!(""));
        } else if block_type == "thinking" {
            block.insert("thinking".to_string(), json!(""));
        } else if block_type == "tool_use" {
            if let Some(tool_id) = id {
                block.insert("id".to_string(), json!(tool_id));
            }
            if let Some(tool_name) = name {
                block.insert("name".to_string(), json!(tool_name));
            }
            block.insert("input".to_string(), json!({}));
        }

        Event::default()
            .event("content_block_start")
            .json_data(json!({
                "type": "content_block_start",
                "index": index,
                "content_block": block
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Legacy `content_block_start` with index=0, type="text" (single-block shell use).
    pub fn content_block_start_simple(&self) -> Event {
        self.content_block_start(0, "text", None, None)
    }

    /// Generate a `content_block_delta` event for text at the given index.
    pub fn text_delta(&self, index: usize, text: &str) -> Event {
        Event::default()
            .event("content_block_delta")
            .json_data(json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {"type": "text_delta", "text": text}
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Legacy `text_delta` at index 0 (single-block shell use).
    pub fn text_delta_simple(&self, text: &str) -> Event {
        self.text_delta(0, text)
    }

    /// Generate a `content_block_delta` event for thinking at the given index.
    pub fn thinking_delta(&self, index: usize, thinking: &str) -> Event {
        Event::default()
            .event("content_block_delta")
            .json_data(json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {"type": "thinking_delta", "thinking": thinking}
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Generate a `content_block_delta` event for JSON tool input at the given index.
    pub fn input_json_delta(&self, index: usize, partial_json: &str) -> Event {
        Event::default()
            .event("content_block_delta")
            .json_data(json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": partial_json
                }
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Generate a `content_block_stop` event at the given index.
    pub fn content_block_stop(&self, index: usize) -> Event {
        Event::default()
            .event("content_block_stop")
            .json_data(json!({
                "type": "content_block_stop",
                "index": index
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Legacy `content_block_stop` at index 0 (single-block shell use).
    pub fn content_block_stop_simple(&self) -> Event {
        self.content_block_stop(0)
    }

    /// Generate the `message_delta` event — sent with stop reason at end of message.
    pub fn message_delta(&self, stop_reason: &str, output_tokens: u32) -> Event {
        Event::default()
            .event("message_delta")
            .json_data(json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                "usage": {"output_tokens": output_tokens}
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Legacy `message_delta` with stop_reason "end_turn" (single-block shell use).
    pub fn message_delta_simple(&self, output_tokens: u32) -> Event {
        self.message_delta("end_turn", output_tokens)
    }

    /// Generate the `message_stop` event — final event in the stream.
    pub fn message_stop(&self) -> Event {
        Event::default()
            .event("message_stop")
            .json_data(json!({
                "type": "message_stop"
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Build a complete non-streaming JSON response body.
    pub fn non_streaming_response(
        &self,
        text: &str,
        input_tokens: u32,
        output_tokens: u32,
    ) -> serde_json::Value {
        json!({
            "id": self.msg_id,
            "type": "message",
            "role": "assistant",
            "model": self.model,
            "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": input_tokens, "output_tokens": output_tokens}
        })
    }
}

/// Helper: build a content_block_stop event without needing a builder.
pub fn emit_block_stop(index: usize) -> Event {
    Event::default()
        .event("content_block_stop")
        .json_data(json!({
            "type": "content_block_stop",
            "index": index
        }))
        .unwrap_or_else(|_| Event::default().data("{}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_creates_events() {
        let builder = SseEventBuilder::new("msg_test".to_string(), "test-model".to_string());

        // All event builder methods should succeed without panic
        let _ = builder.message_start(10);
        let _ = builder.content_block_start(0, "text", None, None);
        let _ = builder.content_block_start(7, "tool_use", Some("toolu_abc"), Some("bash"));
        let _ = builder.text_delta(0, "hello");
        let _ = builder.thinking_delta(0, "thinking...");
        let _ = builder.input_json_delta(0, "{}");
        let _ = builder.content_block_stop(0);
        let _ = builder.message_delta("end_turn", 20);
        let _ = builder.message_stop();
    }

    #[test]
    fn test_non_streaming_response() {
        let builder = SseEventBuilder::new("msg_test".to_string(), "test-model".to_string());
        let resp = builder.non_streaming_response("hello world", 15, 25);

        assert_eq!(resp["id"], "msg_test");
        assert_eq!(resp["model"], "test-model");
        assert_eq!(resp["type"], "message");
        assert_eq!(resp["role"], "assistant");
        assert_eq!(resp["content"][0]["text"], "hello world");
        assert_eq!(resp["stop_reason"], "end_turn");
        assert_eq!(resp["usage"]["input_tokens"], 15);
        assert_eq!(resp["usage"]["output_tokens"], 25);
    }

    #[test]
    fn test_emit_block_stop() {
        let ev = emit_block_stop(3);
        // Event builds without panic
        let _ = ev;
    }
}
