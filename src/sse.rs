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
    pub fn message_start(&self) -> Event {
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
                    "usage": {"input_tokens": 0, "output_tokens": 0}
                }
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Generate the `content_block_start` event — marks the start of a text block.
    pub fn content_block_start(&self) -> Event {
        Event::default()
            .event("content_block_start")
            .json_data(json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""}
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Generate a `content_block_delta` event — a chunk of streamed text.
    pub fn text_delta(&self, text: &str) -> Event {
        Event::default()
            .event("content_block_delta")
            .json_data(json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": text}
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Generate the `content_block_stop` event — marks the end of a text block.
    pub fn content_block_stop(&self) -> Event {
        Event::default()
            .event("content_block_stop")
            .json_data(json!({
                "type": "content_block_stop",
                "index": 0
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Generate the `message_delta` event — sent with stop reason at end of message.
    pub fn message_delta(&self) -> Event {
        Event::default()
            .event("message_delta")
            .json_data(json!({
                "type": "message_delta",
                "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                "usage": {"output_tokens": 0}
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
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
    pub fn non_streaming_response(&self, text: &str) -> serde_json::Value {
        json!({
            "id": self.msg_id,
            "type": "message",
            "role": "assistant",
            "model": self.model,
            "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 0, "output_tokens": 0}
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_creates_events() {
        let builder = SseEventBuilder::new("msg_test".to_string(), "test-model".to_string());

        // All event builder methods should succeed without panic
        let _ = builder.message_start();
        let _ = builder.content_block_start();
        let _ = builder.text_delta("hello");
        let _ = builder.content_block_stop();
        let _ = builder.message_delta();
        let _ = builder.message_stop();
    }

    #[test]
    fn test_non_streaming_response() {
        let builder = SseEventBuilder::new("msg_test".to_string(), "test-model".to_string());
        let resp = builder.non_streaming_response("hello world");

        assert_eq!(resp["id"], "msg_test");
        assert_eq!(resp["model"], "test-model");
        assert_eq!(resp["type"], "message");
        assert_eq!(resp["role"], "assistant");
        assert_eq!(resp["content"][0]["text"], "hello world");
        assert_eq!(resp["stop_reason"], "end_turn");
    }
}
