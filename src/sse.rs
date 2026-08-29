//! SSE (Server-Sent Events) builder for Anthropic-compatible streaming responses.
//!
//! This module eliminates code duplication between shell and OpenCode streaming
//! by providing a unified event builder that constructs properly formatted
//! Anthropic SSE events.
//!
//! # Ordering contract (deployment-gate invariant #1)
//!
//! [`SseEventBuilder`] is a **stateless event factory**: it renders individual
//! frames but does not track stream position. Structural ordering enforcement
//! (no delta without an open block, monotonic block indices, exactly one
//! `message_start`, nothing after a terminal `error`) lives with the consumers
//! — `stream_tracker::SseBlockTracker` plus the per-attempt `StreamContext`
//! flags. A healthy stream must emit exactly:
//!
//! ```text
//! message_start
//!   → content_block_start → content_block_delta* → content_block_stop
//!   → … further blocks with strictly increasing indices …
//!   → message_delta(stop_reason)
//!   → message_stop
//! ```
//!
//! An Anthropic `error` event ([`SseEventBuilder::api_error`]) is terminal: no
//! `message_delta` and no `message_stop` may follow it.

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

    /// Generate the `content_block_start` event — marks the start of a text block.
    ///
    /// # Index discipline
    /// This legacy helper hardcodes `index: 0`. It is only correct for
    /// single-block lifecycles that never open a second block (local shell
    /// echo, session titles). Any stream that can open more than one block —
    /// thinking → text → tool_use transitions, search-intercept retries — must
    /// use [`SseEventBuilder::content_block_start_at`] with indices from
    /// `SseBlockTracker`, or the client receives two blocks claiming index 0
    /// and the second overwrites the first.
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
    ///
    /// # Index discipline
    /// Legacy helper targeting the hardcoded index-0 text block opened by
    /// [`SseEventBuilder::content_block_start`]; see the index-discipline note
    /// there. Use [`SseEventBuilder::text_delta_at`] for any tracked block.
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

    /// Generate a `content_block_delta` event with a specific block index.
    pub fn text_delta_at(&self, index: usize, text: &str) -> Event {
        Event::default()
            .event("content_block_delta")
            .json_data(json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {"type": "text_delta", "text": text}
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Generate a `content_block_delta` event for tool call input JSON.
    pub fn input_json_delta(&self, index: usize, partial_json: &str) -> Event {
        Event::default()
            .event("content_block_delta")
            .json_data(json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {"type": "input_json_delta", "partial_json": partial_json}
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Generate a `content_block_delta` event for thinking/reasoning content.
    pub fn thinking_delta(&self, index: usize, text: &str) -> Event {
        Event::default()
            .event("content_block_delta")
            .json_data(json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {"type": "thinking_delta", "thinking": text}
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Generate the `content_block_stop` event — marks the end of a text block.
    ///
    /// # Index discipline
    /// Legacy helper hardcoding `index: 0`; only valid for single-block
    /// lifecycles. Stopping a tracked (possibly non-zero) block with this
    /// helper emits `content_block_stop {"index": 0}` for an already-closed
    /// index and leaves the real block open — the client renderer then waits
    /// on a block that never stops. Use
    /// [`SseEventBuilder::content_block_stop_at`] instead.
    pub fn content_block_stop(&self) -> Event {
        Event::default()
            .event("content_block_stop")
            .json_data(json!({
                "type": "content_block_stop",
                "index": 0
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Generate a `content_block_start` event for a specific block type and index.
    pub fn content_block_start_at(
        &self,
        index: usize,
        block_type: &str,
        id: Option<&str>,
        name: Option<&str>,
    ) -> Event {
        let mut content_block = json!({
            "type": block_type,
        });
        if block_type == "text" {
            content_block["text"] = json!("");
        }
        if block_type == "thinking" {
            // The Anthropic SDK parses ThinkingBlock as {type, thinking,
            // signature}; without the mandatory "thinking" field the block
            // cannot be constructed and rendering is skipped or shows garbage.
            content_block["thinking"] = json!("");
        }
        if block_type == "tool_use" || block_type == "thinking" {
            if let Some(id_val) = id {
                content_block["id"] = json!(id_val);
            }
            if let Some(name_val) = name {
                content_block["name"] = json!(name_val);
            }
            if block_type == "tool_use" {
                content_block["input"] = json!({});
            }
        }
        Event::default()
            .event("content_block_start")
            .json_data(json!({
                "type": "content_block_start",
                "index": index,
                "content_block": content_block,
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Generate a `content_block_stop` event for a specific block index.
    ///
    /// The indexed counterpart to [`SseEventBuilder::content_block_stop`] and
    /// the correct closer for every block opened via
    /// [`SseEventBuilder::content_block_start_at`].
    pub fn content_block_stop_at(&self, index: usize) -> Event {
        emit_block_stop(index)
    }

    /// Generate the `message_delta` event — sent with stop reason at end of message.
    ///
    /// Only valid after every `content_block_stop` has been emitted and before
    /// `message_stop`; never after a terminal [`SseEventBuilder::api_error`].
    pub fn message_delta(&self, output_tokens: u32) -> Event {
        Event::default()
            .event("message_delta")
            .json_data(json!({
                "type": "message_delta",
                "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                "usage": {"output_tokens": output_tokens}
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Generate a `message_delta` event with a configurable stop reason.
    pub fn message_delta_with_stop(&self, stop_reason: &str, output_tokens: u32) -> Event {
        Event::default()
            .event("message_delta")
            .json_data(json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                "usage": {"output_tokens": output_tokens}
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

    /// Generate the Anthropic `error` event that terminates a stream.
    ///
    /// Per the Messages API spec an error event ends the stream: the server
    /// must not emit `message_delta` or `message_stop` after it. The builder
    /// cannot enforce this (it is stateless), so consumers must treat the
    /// returned frame as the last one they send — see `StreamContext::
    /// error_terminated` for the streaming executor's enforcement flag.
    pub fn api_error(&self, message: &str) -> Event {
        Event::default()
            .event("error")
            .json_data(json!({
                "type": "error",
                "error": {
                    "type": "api_error",
                    "message": message
                }
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))
    }

    /// Generate an arbitrary named SSE event whose data is `payload_json`.
    ///
    /// Generic escape hatch for non-Anthropic event surfaces (dashboard event
    /// bus, admin feeds) whose clients subscribe by event *name* rather than
    /// by Anthropic frame shape. The payload must already be serialized:
    /// callers holding serde enums should serialize them directly, because a
    /// round-trip through [`serde_json::Value`] reorders object fields
    /// alphabetically and would break payloads pinned byte-for-byte.
    ///
    /// # Failure-mode contract (F3)
    /// This method cannot fail, and it never drops the event name. Any future
    /// variant that serializes internally must preserve `.event(name)` in its
    /// serialization-failure fallback instead of degrading to an anonymous
    /// data-only frame — unnamed frames are invisible to named-event
    /// subscribers.
    pub fn named_event(&self, name: &str, payload_json: impl Into<String>) -> Event {
        Event::default().event(name).data(payload_json.into())
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

/// Create a `content_block_stop` event for a specific block index.
///
/// Free-function form of [`SseEventBuilder::content_block_stop_at`], kept for
/// the streaming executor's call sites; both produce identical frames.
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
        let _ = builder.content_block_start();
        let _ = builder.text_delta("hello");
        let _ = builder.content_block_stop();
        let _ = builder.message_delta(20);
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
    fn thinking_block_start_includes_thinking_field() {
        let builder = SseEventBuilder::new("msg_test".to_string(), "test-model".to_string());
        let debug = format!(
            "{:?}",
            builder.content_block_start_at(0, "thinking", None, None)
        );
        // Debug output backslash-escapes the JSON quotes: `\"thinking\":\"\"`.
        assert!(
            debug.contains("\\\"thinking\\\":\\\"\\\""),
            "spec requires content_block_start for thinking to carry \"thinking\": \"\", got: {debug}"
        );
    }

    #[test]
    fn api_error_event_ends_with_error_payload() {
        let builder = SseEventBuilder::new("msg_test".to_string(), "test-model".to_string());
        let debug = format!("{:?}", builder.api_error("upstream exploded"));
        assert!(debug.contains("\\\"type\\\":\\\"error\\\""));
        assert!(debug.contains("\\\"api_error\\\""));
        assert!(debug.contains("upstream exploded"));
    }

    /// Render an event's wire buffer with Debug-escapes removed so assertions
    /// read against the literal SSE framing (`event: X\ndata: {…}`).
    fn plain(event: &Event) -> String {
        format!("{event:?}").replace('\\', "")
    }

    /// Deployment-gate invariant #1, pinned at the unit level: the canonical
    /// single-block lifecycle emits exactly these frames, in this order, with
    /// the required Anthropic fields.
    #[test]
    fn canonical_lifecycle_renders_the_six_invariant_frames_in_order() {
        let builder = SseEventBuilder::new("msg_t".to_string(), "m".to_string());
        let stream = [
            builder.message_start(11),
            builder.content_block_start(),
            builder.text_delta("hello"),
            builder.content_block_stop(),
            builder.message_delta(4),
            builder.message_stop(),
        ];
        let rendered: Vec<String> = stream.iter().map(plain).collect();
        let joined = rendered.join("\u{1}");

        let expected_names = [
            "event: message_start",
            "event: content_block_start",
            "event: content_block_delta",
            "event: content_block_stop",
            "event: message_delta",
            "event: message_stop",
        ];
        let mut cursor = 0;
        for name in expected_names {
            let at = joined
                .find(name)
                .unwrap_or_else(|| panic!("canonical lifecycle missing {name} in:\n{joined:?}"));
            assert!(
                at >= cursor,
                "frame {name} regressed ahead of an earlier frame in:\n{joined:?}"
            );
            cursor = at + name.len();
        }

        // message_start shape: identity, empty content, usage split.
        let start = &rendered[0];
        assert!(start.contains("\"id\":\"msg_t\""), "{start}");
        assert!(start.contains("\"role\":\"assistant\""), "{start}");
        assert!(start.contains("\"input_tokens\":11"), "{start}");
        assert!(start.contains("\"output_tokens\":0"), "{start}");

        // message_delta shape: stop_reason, null stop_sequence, usage.
        let delta = &rendered[4];
        assert!(delta.contains("\"stop_reason\":\"end_turn\""), "{delta}");
        assert!(delta.contains("\"stop_sequence\":null"), "{delta}");
        assert!(delta.contains("\"output_tokens\":4"), "{delta}");
    }

    /// Every indexed emitter must target exactly the block index it was given;
    /// a misrouted delta silently corrupts a different block client-side.
    #[test]
    fn indexed_deltas_and_stops_carry_the_requested_block_index() {
        let builder = SseEventBuilder::new("msg_t".to_string(), "m".to_string());

        let text = plain(&builder.text_delta_at(3, "chunk"));
        assert!(text.contains("event: content_block_delta"), "{text}");
        assert!(text.contains("\"index\":3"), "{text}");
        assert!(text.contains("\"type\":\"text_delta\""), "{text}");

        let json = plain(&builder.input_json_delta(4, "{\"cmd\":\"ls\"}"));
        assert!(json.contains("\"index\":4"), "{json}");
        assert!(json.contains("\"type\":\"input_json_delta\""), "{json}");
        assert!(json.contains("{\"cmd\":\"ls\"}"), "{json}");

        let thinking = plain(&builder.thinking_delta(2, "reasoning"));
        assert!(thinking.contains("\"index\":2"), "{thinking}");
        assert!(
            thinking.contains("\"type\":\"thinking_delta\""),
            "{thinking}"
        );

        let stop = plain(&builder.content_block_stop_at(5));
        assert!(stop.contains("event: content_block_stop"), "{stop}");
        assert!(stop.contains("\"index\":5"), "{stop}");
        // Free-function form must stay byte-identical to the method form.
        assert_eq!(
            plain(&crate::sse::emit_block_stop(5)),
            stop,
            "emit_block_stop drifted from content_block_stop_at"
        );
    }

    /// stop_reason passes through verbatim (end_turn / tool_use / max_tokens),
    /// and stop_sequence stays null per the Messages spec.
    #[test]
    fn message_delta_stop_reasons_pass_through_with_null_stop_sequence() {
        let builder = SseEventBuilder::new("msg_t".to_string(), "m".to_string());
        for reason in ["end_turn", "tool_use", "max_tokens", "stop_sequence"] {
            let frame = plain(&builder.message_delta_with_stop(reason, 9));
            assert!(
                frame.contains(&format!("\"stop_reason\":\"{reason}\"")),
                "{frame}"
            );
            assert!(frame.contains("\"stop_sequence\":null"), "{frame}");
            assert!(frame.contains("\"output_tokens\":9"), "{frame}");
        }
    }

    /// The error frame must be terminal *in shape*: no usage accounting and no
    /// stop_reason that a client could mistake for a clean message end.
    #[test]
    fn api_error_frame_carries_no_delta_or_usage_semantics() {
        let builder = SseEventBuilder::new("msg_t".to_string(), "m".to_string());
        let frame = plain(&builder.api_error("mid-stream failure"));
        assert!(frame.contains("event: error"), "{frame}");
        assert!(frame.contains("\"type\":\"error\""), "{frame}");
        assert!(frame.contains("\"type\":\"api_error\""), "{frame}");
        assert!(frame.contains("mid-stream failure"), "{frame}");
        assert!(!frame.contains("stop_reason"), "{frame}");
        assert!(!frame.contains("output_tokens"), "{frame}");
        assert!(!frame.contains("message_stop"), "{frame}");
    }

    /// Path-A surfaces (dashboard event bus) subscribe by event NAME; the
    /// generic constructor must keep name and payload together in one frame.
    #[test]
    fn named_event_carries_name_and_payload_in_one_frame() {
        let builder = SseEventBuilder::new("msg_t".to_string(), "m".to_string());
        let frame = plain(&builder.named_event("error", r#"{"type":"error"}"#.to_string()));
        assert!(frame.contains("event: error"), "{frame}");
        assert!(frame.contains(r#"data: {"type":"error"}"#), "{frame}");
    }

    /// tool_use blocks open with identity and an empty input object so the
    /// client SDK can construct the block before partial_json arrives.
    #[test]
    fn tool_use_block_start_carries_id_name_and_empty_input() {
        let builder = SseEventBuilder::new("msg_t".to_string(), "m".to_string());
        let frame =
            plain(&builder.content_block_start_at(2, "tool_use", Some("toolu_abc"), Some("Bash")));
        assert!(frame.contains("event: content_block_start"), "{frame}");
        assert!(frame.contains("\"index\":2"), "{frame}");
        assert!(frame.contains("\"type\":\"tool_use\""), "{frame}");
        assert!(frame.contains("\"id\":\"toolu_abc\""), "{frame}");
        assert!(frame.contains("\"name\":\"Bash\""), "{frame}");
        assert!(frame.contains("\"input\":{}"), "{frame}");
    }

    /// The legacy zero-index helpers are pinned to index 0 by contract: if one
    /// ever starts emitting tracked indices, shell/title single-block streams
    /// change meaning silently.
    #[test]
    fn legacy_helpers_target_index_zero() {
        let builder = SseEventBuilder::new("msg_t".to_string(), "m".to_string());
        assert!(
            plain(&builder.content_block_start()).contains("\"index\":0"),
            "content_block_start must keep its documented index-0 contract"
        );
        assert!(
            plain(&builder.text_delta("x")).contains("\"index\":0"),
            "text_delta must keep its documented index-0 contract"
        );
        assert!(
            plain(&builder.content_block_stop()).contains("\"index\":0"),
            "content_block_stop must keep its documented index-0 contract"
        );
    }
}
