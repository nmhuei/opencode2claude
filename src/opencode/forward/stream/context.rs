//! Per-response stream state and OpenAI-to-Anthropic delta translation.

use super::transport::send_sse;
use crate::handlers::MessagesRequest;
use crate::opencode::forward::common::get_correct_tool_name;
use crate::opencode::mapper::is_web_search_tool;
use crate::opencode::sanitize::{parse_dsml_tool_calls, strip_system_tags};
use crate::opencode::types::*;
use crate::sse::SseEventBuilder;
use crate::stream_tracker::SseBlockTracker;
use axum::response::sse::Event;
use tracing::{error, warn};

const MAX_DSML_BUFFER_SIZE: usize = 256 * 1024;

/// Finalize the stream by emitting a fallback text block, message_delta, and message_stop.
///
/// Used on error paths (stream_failed, search loop protection, empty upstream)
/// when `message_start` has already been emitted but no content blocks or
/// `message_stop` have been sent. This ensures the Anthropic SSE protocol
/// sequence is always completed.
pub(super) async fn finalize_stream(
    reason: &str,
    tx: &tokio::sync::mpsc::Sender<Event>,
    builder: &SseEventBuilder,
    tracker: &mut SseBlockTracker,
    ctx: &mut StreamContext,
) {
    if !ctx.message_started {
        // message_start was never emitted — emit a minimal fallback one
        let _ = send_sse(tx, builder.message_start(0)).await;
        ctx.message_started = true;
    }

    // Flush any remaining buffers
    ctx.flush_remaining(tracker, tx, builder, &Default::default())
        .await;

    // Close any remaining active blocks
    for (_, idx) in tracker.close_all() {
        let _ = send_sse(tx, crate::sse::emit_block_stop(idx)).await;
    }

    // If no content block was ever opened, emit an error event instead of a fake system text block
    // to prevent client-side model output validation crashes.
    if !tracker.has_any_blocks_ever_opened() {
        let error_msg = format!(
            "Upstream response did not contain content blocks (reason: {})",
            reason
        );
        let error_ev = Event::default()
            .event("error")
            .json_data(serde_json::json!({
                "type": "error",
                "error": {
                    "type": "api_error",
                    "message": error_msg
                }
            }))
            .unwrap_or_else(|_| Event::default().data("{}"));
        let _ = send_sse(tx, error_ev).await;
    } else {
        let stop_reason = if ctx.has_emitted_tool_use {
            "tool_use".to_string()
        } else {
            "end_turn".to_string()
        };

        let _ = send_sse(tx, builder.message_delta_with_stop(&stop_reason, 1)).await;
    }

    let _ = send_sse(tx, builder.message_stop()).await;
}

pub(super) async fn process_openai_sse_line(
    line: &str,
    ctx: &mut StreamContext,
    tracker: &mut SseBlockTracker,
    tx: &tokio::sync::mpsc::Sender<Event>,
    builder: &SseEventBuilder,
    payload: &MessagesRequest,
) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return false;
    }

    let Some(stripped) = line.strip_prefix("data:") else {
        return false;
    };
    let data_str = stripped.trim();
    if data_str == "[DONE]" {
        return true;
    }

    let chunk = match serde_json::from_str::<OpenAiStreamChunk>(data_str) {
        Ok(chunk) => chunk,
        Err(e) => {
            warn!(
                "Ignoring malformed upstream SSE data line: {} ({})",
                data_str.chars().take(120).collect::<String>(),
                e
            );
            return false;
        }
    };

    if let Some(choice) = chunk.choices.first() {
        if let Some(reason) = &choice.finish_reason {
            ctx.update_stop_reason(reason);
        }

        if let Some(reasoning) = &choice.delta.reasoning_content {
            ctx.process_reasoning_delta(reasoning, tracker, tx, builder)
                .await;
        }

        if let Some(content) = &choice.delta.content {
            ctx.process_content_delta(content, tracker, tx, builder, payload)
                .await;
        }

        if let Some(tool_calls) = &choice.delta.tool_calls {
            ctx.process_tool_calls(tool_calls, tracker, tx, builder, payload)
                .await;
        }
    }

    false
}

/// Bundles all mutable streaming state for `forward_to_llm_stream`.
///
/// Encapsulates the 11+ mutable variables that track search interception,
/// DSML parsing, text accumulation, and stream health into a single unit.
/// Methods on this struct handle the core processing logic for each
/// streaming event type (reasoning deltas, text deltas with DSML detection,
/// tool call deltas, and final buffer flushing).
pub(super) struct StreamContext {
    /// Whether `message_start` has been emitted.
    pub(super) message_started: bool,
    /// Whether we are currently intercepting a web search tool call.
    pub(super) intercepting_search: bool,
    /// Tool call ID for the intercepted search, if any.
    pub(super) search_tc_id: String,
    /// Tool call name for the intercepted search, if any.
    pub(super) search_tc_name: String,
    /// Accumulated JSON arguments for the intercepted search tool call.
    pub(super) search_tc_args: String,
    /// Accumulated thinking text across all chunks in this response turn.
    pub(super) accumulated_thinking: String,
    /// Accumulated visible text across all chunks in this response turn.
    pub(super) accumulated_text: String,
    /// Whether the stream encountered a fatal read error.
    pub(super) stream_failed: bool,
    /// Whether any `tool_use` content block has been emitted.
    pub(super) has_emitted_tool_use: bool,
    /// Whether we are currently inside a <｜DSML｜tool_calls> block.
    pub(super) dsml_mode: bool,
    /// Buffer for DSML content being accumulated inside a <｜DSML｜tool_calls> block.
    pub(super) dsml_stream_buffer: String,
    /// Buffer for text content before DSML tag detection or after DSML parsing.
    pub(super) text_stream_buffer: String,
    /// Determined from `finish_reason` in the last stream chunk.
    pub(super) final_stop_reason: String,
    /// Whether this is a compaction/summarization request.
    pub(super) is_compact: bool,
}

impl StreamContext {
    pub(super) fn new(is_compact: bool) -> Self {
        Self {
            message_started: false,
            intercepting_search: false,
            search_tc_id: String::new(),
            search_tc_name: String::new(),
            search_tc_args: String::new(),
            accumulated_thinking: String::new(),
            accumulated_text: String::new(),
            stream_failed: false,
            has_emitted_tool_use: false,
            dsml_mode: false,
            dsml_stream_buffer: String::new(),
            text_stream_buffer: String::new(),
            final_stop_reason: "end_turn".to_string(),
            is_compact,
        }
    }

    /// Update the final stop reason from a stream chunk's `finish_reason`.
    fn update_stop_reason(&mut self, reason: &str) {
        self.final_stop_reason = match reason {
            "stop" => "end_turn".to_string(),
            "tool_calls" => "tool_use".to_string(),
            "length" => "max_tokens".to_string(),
            _ => "end_turn".to_string(),
        };
    }

    /// Process a reasoning/thinking delta from the upstream stream chunk.
    ///
    /// Appends the reasoning text to the accumulated buffer and, unless we are
    /// currently intercepting a search tool call, emits SSE `thinking_delta`
    /// events. Creates a new thinking content block if one is not already open.
    async fn process_reasoning_delta(
        &mut self,
        reasoning: &str,
        tracker: &mut SseBlockTracker,
        tx: &tokio::sync::mpsc::Sender<Event>,
        builder: &SseEventBuilder,
    ) {
        if reasoning.is_empty() {
            return;
        }
        self.accumulated_thinking.push_str(reasoning);
        if self.intercepting_search {
            return;
        }

        let (thinking_idx, thinking_is_new, closed_text) = tracker.ensure_thinking();
        if let Some(closed) = closed_text {
            let _ = tx.send(crate::sse::emit_block_stop(closed)).await;
        }
        if thinking_is_new {
            let _ = tx
                .send(builder.content_block_start_at(thinking_idx, "thinking", None, None))
                .await;
        }
        let _ = tx
            .send(builder.thinking_delta(thinking_idx, reasoning))
            .await;
    }

    /// Process a content/text delta from the upstream stream chunk.
    ///
    /// Handles DSML mode detection and parsing: when not in DSML mode, text is
    /// accumulated and checked for the <｜DSML｜tool_calls> opening tag. When in
    /// DSML mode, content is buffered and checked for the closing tag. When the
    /// closing tag is found, DSML tool calls are parsed and emitted as `tool_use`
    /// content blocks. Emits `text_delta` events for non-DSML text content.
    async fn process_content_delta(
        &mut self,
        content: &str,
        tracker: &mut SseBlockTracker,
        tx: &tokio::sync::mpsc::Sender<Event>,
        builder: &SseEventBuilder,
        payload: &MessagesRequest,
    ) {
        // Step 1: Accumulate into the appropriate buffer
        if self.dsml_mode {
            self.dsml_stream_buffer.push_str(content);
            // Enforce DSML buffer cap — prevents OOM from long text prefix
            if self.dsml_stream_buffer.len() > MAX_DSML_BUFFER_SIZE {
                error!(
                    "DSML stream buffer exceeded {} bytes — truncating",
                    MAX_DSML_BUFFER_SIZE
                );
                self.dsml_stream_buffer = String::new();
                self.dsml_mode = false;
            }
            // Check for closing DSML tag
            if let Some(end_pos) = self
                .dsml_stream_buffer
                .find("</\u{ff5c}DSML\u{ff5c}tool_calls>")
            {
                let end_idx = end_pos + "</\u{ff5c}DSML\u{ff5c}tool_calls>".len();
                let dsml_block = &self.dsml_stream_buffer[..end_idx];
                let remaining = self.dsml_stream_buffer[end_idx..].to_string();

                let calls = parse_dsml_tool_calls(dsml_block);
                for call in calls {
                    self.has_emitted_tool_use = true;
                    let call_idx = tracker.next_index();
                    let tool_id = format!(
                        "toolu_dsml_{}_{}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis(),
                        call_idx
                    );

                    if let Some(idx) = tracker.close_thinking() {
                        let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
                    }
                    if let Some(idx) = tracker.close_text() {
                        let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
                    }

                    let _ = tx
                        .send(builder.content_block_start_at(
                            call_idx,
                            "tool_use",
                            Some(&tool_id),
                            Some(&get_correct_tool_name(&call.name, payload)),
                        ))
                        .await;

                    let args_str = serde_json::to_string(&call.arguments).unwrap_or_default();
                    let _ = tx.send(builder.input_json_delta(call_idx, &args_str)).await;

                    let _ = tx.send(crate::sse::emit_block_stop(call_idx)).await;
                }

                self.dsml_stream_buffer = String::new();
                self.dsml_mode = false;

                if !remaining.is_empty() {
                    self.text_stream_buffer.push_str(&remaining);
                }
            }
        } else {
            self.text_stream_buffer.push_str(content);
        }

        // Step 2: If not in DSML mode, check text buffer for DSML opening tag
        if !self.dsml_mode {
            if let Some(start_pos) = self
                .text_stream_buffer
                .find("<\u{ff5c}DSML\u{ff5c}tool_calls>")
            {
                let text_to_yield = &self.text_stream_buffer[..start_pos];
                let remainder = &self.text_stream_buffer[start_pos..];

                let cleaned = if self.is_compact {
                    text_to_yield.to_string()
                } else {
                    strip_system_tags(text_to_yield)
                };
                if !cleaned.is_empty() {
                    self.accumulated_text.push_str(&cleaned);
                    if !self.intercepting_search {
                        if let Some(idx) = tracker.close_thinking() {
                            let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
                        }

                        let (text_idx, text_is_new, _closed) = tracker.ensure_text();
                        if text_is_new {
                            let _ = tx
                                .send(builder.content_block_start_at(text_idx, "text", None, None))
                                .await;
                        }
                        let _ = tx.send(builder.text_delta_at(text_idx, &cleaned)).await;
                    }
                }

                self.dsml_mode = true;
                self.dsml_stream_buffer = remainder.to_string();
                self.text_stream_buffer = String::new();
            } else {
                let (to_yield, pending) = split_pending_text(&self.text_stream_buffer);
                let cleaned = if self.is_compact {
                    to_yield.to_string()
                } else {
                    strip_system_tags(&to_yield)
                };
                if !cleaned.is_empty() {
                    self.accumulated_text.push_str(&cleaned);
                    if !self.intercepting_search {
                        if let Some(idx) = tracker.close_thinking() {
                            let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
                        }

                        let (text_idx, text_is_new, _closed) = tracker.ensure_text();
                        if text_is_new {
                            let _ = tx
                                .send(builder.content_block_start_at(text_idx, "text", None, None))
                                .await;
                        }
                        let _ = tx.send(builder.text_delta_at(text_idx, &cleaned)).await;
                    }
                }
                self.text_stream_buffer = pending;
            }
        }
    }

    /// Process tool call deltas from the upstream stream chunk.
    ///
    /// For web search tools, sets `intercepting_search` flags and accumulates
    /// JSON arguments. For regular tool calls, opens a `tool_use` content block
    /// and emits `input_json_delta` events for the streaming arguments.
    async fn process_tool_calls(
        &mut self,
        tool_calls: &[OpenAiStreamToolCall],
        tracker: &mut SseBlockTracker,
        tx: &tokio::sync::mpsc::Sender<Event>,
        builder: &SseEventBuilder,
        payload: &MessagesRequest,
    ) {
        for tc in tool_calls {
            let call_idx = tc.index;

            // If not created yet and we have tool id & function name
            #[allow(clippy::map_entry)]
            if tracker.tool_idx(call_idx).is_none() {
                if let (Some(id), Some(func)) = (&tc.id, &tc.function) {
                    if let Some(name) = &func.name {
                        if is_web_search_tool(name) {
                            self.intercepting_search = true;
                            self.search_tc_id = id.clone();
                            self.search_tc_name = name.clone();
                        } else {
                            // Close thinking block if open
                            if let Some(idx) = tracker.close_thinking() {
                                let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
                            }
                            if let Some(idx) = tracker.close_text() {
                                let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
                            }

                            let (_block_idx, _closed_t, _closed_x) = tracker.open_tool_use(
                                call_idx,
                                id.clone(),
                                get_correct_tool_name(name, payload),
                            );

                            let _ = tx
                                .send(builder.content_block_start_at(
                                    _block_idx,
                                    "tool_use",
                                    Some(id),
                                    Some(&get_correct_tool_name(name, payload)),
                                ))
                                .await;
                            self.has_emitted_tool_use = true;
                        }
                    }
                }
            }

            // Send arguments delta if present
            if self.intercepting_search {
                if let Some(func) = &tc.function {
                    if let Some(args) = &func.arguments {
                        self.search_tc_args.push_str(args);
                    }
                }
            } else if let Some((idx, _, _)) = tracker.tool_idx(call_idx) {
                if let Some(func) = &tc.function {
                    if let Some(args) = &func.arguments {
                        if !args.is_empty() {
                            let _ = tx.send(builder.input_json_delta(*idx, args)).await;
                        }
                    }
                }
            }
        }
    }

    /// Flush any remaining text and DSML buffers at the end of the stream.
    ///
    /// When the stream ends (either naturally or before a search-interception
    /// loop), the text and DSML buffers may contain unprocessed content. This
    /// method flushes the text buffer as a final `text_delta`, parses any
    /// remaining DSML block for tool calls, and closes all active content blocks.
    pub(super) async fn flush_remaining(
        &mut self,
        tracker: &mut SseBlockTracker,
        tx: &tokio::sync::mpsc::Sender<Event>,
        builder: &SseEventBuilder,
        payload: &MessagesRequest,
    ) {
        // Flush any remaining text in text_stream_buffer
        let cleaned = if self.is_compact {
            self.text_stream_buffer.clone()
        } else {
            strip_system_tags(&self.text_stream_buffer)
        };
        if !cleaned.is_empty() {
            self.accumulated_text.push_str(&cleaned);
            if !self.intercepting_search {
                if let Some(idx) = tracker.close_thinking() {
                    let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
                }

                let (text_idx, text_is_new, _closed) = tracker.ensure_text();
                if text_is_new {
                    let _ = tx
                        .send(builder.content_block_start_at(text_idx, "text", None, None))
                        .await;
                }
                let _ = tx.send(builder.text_delta_at(text_idx, &cleaned)).await;
            }
        }

        // Flush/parse any remaining unclosed DSML block in dsml_stream_buffer
        if self.dsml_mode && !self.dsml_stream_buffer.is_empty() {
            let calls = parse_dsml_tool_calls(&self.dsml_stream_buffer);
            for call in calls {
                self.has_emitted_tool_use = true;
                let call_idx = tracker.next_index();
                let tool_id = format!(
                    "toolu_dsml_{}_{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis(),
                    call_idx
                );

                if let Some(idx) = tracker.close_thinking() {
                    let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
                }
                if let Some(idx) = tracker.close_text() {
                    let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
                }

                let _ = tx
                    .send(builder.content_block_start_at(
                        call_idx,
                        "tool_use",
                        Some(&tool_id),
                        Some(&get_correct_tool_name(&call.name, payload)),
                    ))
                    .await;

                let args_str = serde_json::to_string(&call.arguments).unwrap_or_default();
                let _ = tx.send(builder.input_json_delta(call_idx, &args_str)).await;

                let _ = tx.send(crate::sse::emit_block_stop(call_idx)).await;
            }
        }
    }
}

pub(super) fn split_pending_text(text: &str) -> (String, String) {
    let tag = "<｜DSML｜tool_calls>";
    for i in (1..=tag.len()).rev() {
        if tag.is_char_boundary(i) {
            let prefix = &tag[..i];
            if text.ends_with(prefix) {
                let split_idx = text.len() - prefix.len();
                return (text[..split_idx].to_string(), prefix.to_string());
            }
        }
    }
    (text.to_string(), String::new())
}
