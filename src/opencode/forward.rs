//! Forwarding logic for communicating with the upstream OpenAI-compatible API.
//!
//! Handles synchronous and streaming requests, search tool interception,
//! WARP IP rotation for rate-limit retry, and SSE event construction.

use crate::error::BridgeError;
use crate::handlers::{ContentVal, MessagesRequest};
use crate::opencode::mapper::{extract_search_query, is_web_search_tool, map_anthropic_to_openai, is_compact_request};
use crate::opencode::retry::execute_with_warp_retry;
use crate::opencode::sanitize::{extract_and_clean_dsml, parse_dsml_tool_calls, strip_system_tags};
use crate::opencode::search::SearchClient;
use crate::opencode::types::*;
use crate::sse::SseEventBuilder;
use crate::state::AppState;
use crate::stream_tracker::SseBlockTracker;
use axum::response::sse::Event;
use futures_util::{Stream, StreamExt};
use reqwest::Client;
use std::convert::Infallible;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Timeout for sending a single SSE event through the mpsc channel.
/// Prevents the stream task from hanging forever if the receiver is slow.
const SSE_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Check if the OpenCode daemon is running and reachable.
pub async fn check_daemon(client: &Client, port: u16) -> bool {
    let url = format!("http://127.0.0.1:{}/doc", port);
    client
        .get(&url)
        .timeout(std::time::Duration::from_millis(500))
        .send()
        .await
        .is_ok()
}

/// Maximum size of the DSML streaming pre-buffer (256KB).
/// Prevents unbounded memory growth from long text prefix before the
/// closing <|DSML|tool_calls> tag is found.
const MAX_DSML_BUFFER_SIZE: usize = 256 * 1024;

/// Wraps a stream and cancels a CancellationToken when dropped.
/// This ensures the spawned task is notified when the client disconnects.
struct DropCancel {
    token: CancellationToken,
    inner: std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>,
}

impl Drop for DropCancel {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

impl Stream for DropCancel {
    type Item = Result<Event, Infallible>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

/// Inject search results into the conversation history (both sync and stream paths).
///
/// Appends an assistant turn (with thinking, text, and tool_use blocks) followed by
/// a tool_result turn. Used by `forward_to_llm_sync` and `forward_to_llm_stream`
/// after intercepting a web search tool call.
fn inject_search_results(
    payload: &mut MessagesRequest,
    search_results: &str,
    thinking: &str,
    text: &str,
    search_tc_id: &str,
    search_tc_name: &str,
    search_tc_input: &serde_json::Value,
) {
    let mut assistant_content = Vec::new();

    if !thinking.is_empty() {
        assistant_content.push(
            serde_json::from_value(serde_json::json!({
                "type": "text",
                "text": format!("<thinking>{}</thinking>", thinking)
            }))
            .unwrap(),
        );
    }
    if !text.is_empty() {
        assistant_content.push(
            serde_json::from_value(serde_json::json!({
                "type": "text",
                "text": text
            }))
            .unwrap(),
        );
    }
    assistant_content.push(
        serde_json::from_value(serde_json::json!({
            "type": "tool_use",
            "id": search_tc_id,
            "name": search_tc_name,
            "input": search_tc_input
        }))
        .unwrap(),
    );

    payload.messages.push(crate::handlers::Message {
        role: "assistant".to_string(),
        content: ContentVal::Multiple(assistant_content),
    });

    // Append tool response turn
    let tool_result_content = vec![serde_json::from_value(serde_json::json!({
        "type": "tool_result",
        "tool_use_id": search_tc_id,
        "name": search_tc_name,
        "content": search_results
    }))
    .unwrap()];
    payload.messages.push(crate::handlers::Message {
        role: "user".to_string(),
        content: ContentVal::Multiple(tool_result_content),
    });
}

/// Send an SSE event through the mpsc channel with a timeout.
/// Returns `true` if the event was sent, `false` if the channel timed out or closed.
async fn send_sse(tx: &tokio::sync::mpsc::Sender<Event>, event: Event) -> bool {
    match tokio::time::timeout(SSE_SEND_TIMEOUT, tx.send(event)).await {
        Ok(Ok(())) => true,
        Ok(Err(_)) => {
            warn!("SSE send failed because receiver was closed");
            false
        }
        Err(_) => {
            warn!("SSE send timed out after {:?}", SSE_SEND_TIMEOUT);
            false
        }
    }
}

/// Finalize the stream by emitting a fallback text block, message_delta, and message_stop.
///
/// Used on error paths (stream_failed, search loop protection, empty upstream)
/// when `message_start` has already been emitted but no content blocks or
/// `message_stop` have been sent. This ensures the Anthropic SSE protocol
/// sequence is always completed.
async fn finalize_stream(
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

async fn process_openai_sse_line(
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
struct StreamContext {
    /// Whether `message_start` has been emitted.
    message_started: bool,
    /// Whether we are currently intercepting a web search tool call.
    intercepting_search: bool,
    /// Tool call ID for the intercepted search, if any.
    search_tc_id: String,
    /// Tool call name for the intercepted search, if any.
    search_tc_name: String,
    /// Accumulated JSON arguments for the intercepted search tool call.
    search_tc_args: String,
    /// Accumulated thinking text across all chunks in this response turn.
    accumulated_thinking: String,
    /// Accumulated visible text across all chunks in this response turn.
    accumulated_text: String,
    /// Whether the stream encountered a fatal read error.
    stream_failed: bool,
    /// Whether any `tool_use` content block has been emitted.
    has_emitted_tool_use: bool,
    /// Whether we are currently inside a <｜DSML｜tool_calls> block.
    dsml_mode: bool,
    /// Buffer for DSML content being accumulated inside a <｜DSML｜tool_calls> block.
    dsml_stream_buffer: String,
    /// Buffer for text content before DSML tag detection or after DSML parsing.
    text_stream_buffer: String,
    /// Determined from `finish_reason` in the last stream chunk.
    final_stop_reason: String,
    /// Whether this is a compaction/summarization request.
    is_compact: bool,
}

impl StreamContext {
    fn new(is_compact: bool) -> Self {
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

                let cleaned = if self.is_compact { text_to_yield.to_string() } else { strip_system_tags(text_to_yield) };
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
                let cleaned = if self.is_compact { to_yield.to_string() } else { strip_system_tags(&to_yield) };
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
    async fn flush_remaining(
        &mut self,
        tracker: &mut SseBlockTracker,
        tx: &tokio::sync::mpsc::Sender<Event>,
        builder: &SseEventBuilder,
        payload: &MessagesRequest,
    ) {
        // Flush any remaining text in text_stream_buffer
        let cleaned = if self.is_compact { self.text_stream_buffer.clone() } else { strip_system_tags(&self.text_stream_buffer) };
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

// ── API Forwarding Implementations ──

pub async fn forward_to_llm_sync(
    state: &AppState,
    api_key: String,
    mut payload: MessagesRequest,
    model: String,
    search_client: SearchClient,
    max_search_loops: u32,
) -> Result<serde_json::Value, BridgeError> {
    let mut loop_count = 0;
    loop {
        loop_count += 1;
        if loop_count > max_search_loops {
            return Err(BridgeError::UpstreamError(
                "Search loop protection triggered".to_string(),
            ));
        }

        let openai_req = map_anthropic_to_openai(&payload, model.clone());

        info!("Forwarding sync request for model {}", model);

        let res = execute_with_warp_retry(state, &api_key, &openai_req).await?;

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            error!(
                "Upstream API returned status {}: {} (truncated)",
                status,
                body.chars().take(300).collect::<String>()
            );
            return Err(BridgeError::UpstreamError(format!(
                "Upstream returned status {}",
                status
            )));
        }

        let openai_resp: OpenAiResponse = res
            .json()
            .await
            .map_err(|e| BridgeError::UpstreamError(format!("Failed to parse response: {}", e)))?;

        let choice = openai_resp.choices.first().ok_or_else(|| {
            BridgeError::UpstreamError("No choices returned from upstream".to_string())
        })?;

        // Extract DSML tool calls and clean the message content
        let mut dsml_tool_calls = Vec::new();
        let mut cleaned_message_content = choice.message.content.clone();
        let mut has_search = false;
        let mut search_tc_id = String::new();
        let mut search_tc_name = String::new();
        let mut search_tc_input = serde_json::Value::Null;
        let mut search_query = String::new();

        if let Some(text) = &choice.message.content {
            let is_compact = is_compact_request(&payload);
            if is_compact {
                cleaned_message_content = Some(text.clone());
            } else {
                let (cleaned, calls) = extract_and_clean_dsml(text);
                cleaned_message_content = Some(cleaned);
                dsml_tool_calls = calls;
            }
        }

        // Check if there is an intercepted search tool call (native first, then DSML)
        if let Some(tool_calls) = &choice.message.tool_calls {
            for tc in tool_calls {
                if is_web_search_tool(&tc.function.name) {
                    has_search = true;
                    search_tc_id = tc.id.clone();
                    search_tc_name = tc.function.name.clone();
                    let input_val: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    search_tc_input = input_val;
                    search_query = extract_search_query(&tc.function.arguments);
                    break;
                }
            }
        }

        if !has_search {
            for (i, call) in dsml_tool_calls.iter().enumerate() {
                if is_web_search_tool(&call.name) {
                    has_search = true;
                    search_tc_id = format!(
                        "toolu_dsml_{}_{}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis(),
                        i
                    );
                    search_tc_name = call.name.clone();
                    search_tc_input = call.arguments.clone();
                    let args_str = serde_json::to_string(&call.arguments).unwrap_or_default();
                    search_query = extract_search_query(&args_str);
                    break;
                }
            }
        }

        if has_search {
            info!(
                "Intercepted sync search tool call. Query: '{}'",
                search_query
            );
            let search_results = search_client.search(&search_query).await;
            info!("Search completed. Results length: {}", search_results.len());

            // Pre-strip system tags from the cleaned message content
            let is_compact = is_compact_request(&payload);
            let text_cleaned = cleaned_message_content
                .as_deref()
                .map(|t| if is_compact { t.to_string() } else { strip_system_tags(t) })
                .unwrap_or_default();

            inject_search_results(
                &mut payload,
                &search_results,
                choice.message.reasoning_content.as_deref().unwrap_or(""),
                &text_cleaned,
                &search_tc_id,
                &search_tc_name,
                &search_tc_input,
            );

            // Loop again with updated history
            continue;
        }

        // Standard response formatting (no search intercepted or final turn)
        let mut content_blocks = Vec::new();

        // 1. Thinking block (reasoning_content)
        if let Some(reasoning) = &choice.message.reasoning_content {
            if !reasoning.is_empty() {
                content_blocks.push(serde_json::json!({
                    "type": "thinking",
                    "thinking": reasoning
                }));
            }
        }

        // 2. Text block
        if let Some(text) = &cleaned_message_content {
            let is_compact = is_compact_request(&payload);
            let cleaned = if is_compact { text.to_string() } else { strip_system_tags(text) };
            if !cleaned.is_empty() {
                content_blocks.push(serde_json::json!({
                    "type": "text",
                    "text": cleaned
                }));
            }
        }

        // 3. Native Tool calls
        let mut has_tool_calls = false;
        if let Some(tool_calls) = &choice.message.tool_calls {
            for tc in tool_calls {
                has_tool_calls = true;
                let input_val: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                content_blocks.push(serde_json::json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.function.name,
                    "input": input_val
                }));
            }
        }

        // 4. DSML Tool calls
        for (i, call) in dsml_tool_calls.into_iter().enumerate() {
            has_tool_calls = true;
            let tool_id = format!(
                "toolu_dsml_{}_{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
                i
            );
            let cased_name = get_correct_tool_name(&call.name, &payload);
            content_blocks.push(serde_json::json!({
                "type": "tool_use",
                "id": tool_id,
                "name": cased_name,
                "input": call.arguments
            }));
        }

        // Ensure we always have at least one text or tool_use block in the content list
        // to prevent Anthropic's client-side validation from crashing.
        let has_visible_content = content_blocks.iter().any(|block| {
            let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
            block_type == "text" || block_type == "tool_use"
        });
        if !has_visible_content {
            content_blocks.push(serde_json::json!({
                "type": "text",
                "text": "[Empty upstream response]"
            }));
        }

        let stop_reason = match choice.finish_reason.as_deref() {
            Some("stop") => {
                if has_tool_calls {
                    "tool_use"
                } else {
                    "end_turn"
                }
            }
            Some("tool_calls") => "tool_use",
            Some("length") => "max_tokens",
            _ => {
                if has_tool_calls {
                    "tool_use"
                } else {
                    "end_turn"
                }
            }
        };

        let usage = openai_resp.usage.unwrap_or(OpenAiUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
        });

        let anthropic_resp = serde_json::json!({
            "id": format!("msg_opencode_{}", openai_resp.id),
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": content_blocks,
            "stop_reason": stop_reason,
            "stop_sequence": null,
            "usage": {
                "input_tokens": usage.prompt_tokens,
                "output_tokens": usage.completion_tokens
            }
        });

        return Ok(anthropic_resp);
    }
}

/// Perform a streaming completions request to upstream OpenCode API and stream Anthropic SSE chunks.
pub async fn forward_to_llm_stream(
    state: &AppState,
    api_key: String,
    payload: MessagesRequest,
    model: String,
    channel_capacity: usize,
    search_client: SearchClient,
    max_search_loops: u32,
) -> Result<impl Stream<Item = Result<Event, Infallible>>, BridgeError> {
    let (tx, rx) = tokio::sync::mpsc::channel(channel_capacity);
    let cancel_token = CancellationToken::new();
    let msg_id = format!(
        "msg_opencode_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let builder = SseEventBuilder::new(msg_id, model.clone());
    let state_clone = state.clone();
    let api_key_clone = api_key;
    let model_clone = model.clone();

    tokio::spawn({
        // Clone the token so the spawn and DropCancel share the same cancellation state
        let cancel_token_spawn = cancel_token.clone();
        async move {
            let mut current_payload = payload;
            let mut loop_count = 0;
            let mut message_emitted = false;
            let mut tracker = SseBlockTracker::new();

            loop {
                // Check if client disconnected
                if cancel_token_spawn.is_cancelled() {
                    info!("Client disconnected \u{2014} cancelling streaming task");
                    break;
                }

                loop_count += 1;
                let is_compact = is_compact_request(&current_payload);
                if loop_count > max_search_loops {
                    error!("Search loop protection triggered!");
                    // Build a temporary context for finalization if we emitted message_start
                    let mut tmp_ctx = StreamContext::new(is_compact);
                    tmp_ctx.message_started = message_emitted;
                    finalize_stream(
                        "search_loop_protection",
                        &tx,
                        &builder,
                        &mut tracker,
                        &mut tmp_ctx,
                    )
                    .await;
                    break;
                }

                let openai_req = map_anthropic_to_openai(&current_payload, model_clone.clone());

                info!(
                    "Forwarding stream request for model {} (loop {})",
                    model_clone, loop_count
                );

                let res = match execute_with_warp_retry(&state_clone, &api_key_clone, &openai_req)
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        error!("Error forwarding upstream request: {}", e);
                        // Send error SSE event so Claude Code gets a clear message
                        let error_ev = Event::default()
                            .event("error")
                            .json_data(serde_json::json!({
                                "type": "error",
                                "error": {
                                    "type": "api_error",
                                    "message": format!("Bridge upstream error: {}", e)
                                }
                            }))
                            .unwrap_or_else(|_| Event::default().data("{}"));
                        let _ = send_sse(&tx, error_ev).await;
                        // Finalize to ensure SSE protocol completes
                        let mut tmp_tracker = SseBlockTracker::new();
                        let mut tmp_ctx = StreamContext::new(is_compact);
                        // Use message_emitted from outer scope so we don't emit
                        // a duplicate message_start if this is a search loop iteration
                        tmp_ctx.message_started = message_emitted;
                        finalize_stream("api_error", &tx, &builder, &mut tmp_tracker, &mut tmp_ctx)
                            .await;
                        break;
                    }
                };

                if !res.status().is_success() {
                    let status = res.status();
                    let body = res.text().await.unwrap_or_default();
                    error!(
                        "Upstream API returned status {}: {} (truncated)",
                        status,
                        body.chars().take(300).collect::<String>()
                    );
                    // Send error SSE event with status only (no body leak to client)
                    let error_ev = Event::default()
                        .event("error")
                        .json_data(serde_json::json!({
                            "type": "error",
                            "error": {
                                "type": "api_error",
                                "message": format!("Upstream returned {}", status)
                            }
                        }))
                        .unwrap_or_else(|_| Event::default().data("{}"));
                    let _ = send_sse(&tx, error_ev).await;
                    // Finalize to ensure SSE protocol completes (message_stop after error)
                    let mut tmp_tracker = SseBlockTracker::new();
                    let mut tmp_ctx = StreamContext::new(is_compact);
                    tmp_ctx.message_started = message_emitted;
                    finalize_stream(
                        "upstream_non_2xx",
                        &tx,
                        &builder,
                        &mut tmp_tracker,
                        &mut tmp_ctx,
                    )
                    .await;
                    break;
                }

                let mut bytes_stream = res.bytes_stream();
                let mut line_buffer = Vec::new();
                let mut stream_done = false;

                let mut ctx = StreamContext::new(is_compact);

                if loop_count == 1 {
                    let input_tokens = estimate_input_tokens(&current_payload);
                    let _ = send_sse(&tx, builder.message_start(input_tokens)).await;
                    ctx.message_started = true;
                    message_emitted = true;
                } else {
                    // On search intercept loops (loop_count > 1), message_start was already
                    // emitted in the first iteration
                    ctx.message_started = true;
                }

                while let Some(chunk_res) = bytes_stream.next().await {
                    let chunk = match chunk_res {
                        Ok(c) => c,
                        Err(e) => {
                            error!("Error reading chunk from upstream: {}", e);
                            ctx.stream_failed = true;
                            break;
                        }
                    };
                    line_buffer.extend_from_slice(&chunk);

                    while let Some(pos) = line_buffer.iter().position(|&b| b == b'\n') {
                        let line_bytes = line_buffer.drain(..pos + 1).collect::<Vec<u8>>();
                        let line = String::from_utf8_lossy(&line_bytes);
                        if process_openai_sse_line(
                            &line,
                            &mut ctx,
                            &mut tracker,
                            &tx,
                            &builder,
                            &current_payload,
                        )
                        .await
                        {
                            stream_done = true;
                            break;
                        }
                    }

                    if stream_done {
                        break;
                    }
                }

                // Do not drop a final SSE line if upstream closes without trailing newline.
                if !stream_done && !line_buffer.is_empty() {
                    let line = String::from_utf8_lossy(&line_buffer);
                    let _ = process_openai_sse_line(
                        &line,
                        &mut ctx,
                        &mut tracker,
                        &tx,
                        &builder,
                        &current_payload,
                    )
                    .await;
                    line_buffer.clear();
                }

                if ctx.stream_failed {
                    error!("Stream failed — finalizing stream");
                    finalize_stream("upstream_read_error", &tx, &builder, &mut tracker, &mut ctx)
                        .await;
                    break;
                }

                if ctx.intercepting_search {
                    // Extract query from accumulated arguments
                    let input_val: serde_json::Value = serde_json::from_str(&ctx.search_tc_args)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    let search_query = extract_search_query(&ctx.search_tc_args);

                    info!(
                        "Intercepted stream search tool call. Query: '{}'",
                        search_query
                    );
                    let search_results = search_client.search(&search_query).await;
                    info!("Search completed. Results length: {}", search_results.len());

                    inject_search_results(
                        &mut current_payload,
                        &search_results,
                        &ctx.accumulated_thinking,
                        &ctx.accumulated_text,
                        &ctx.search_tc_id,
                        &ctx.search_tc_name,
                        &input_val,
                    );

                    // The first loop may have emitted thinking/text blocks before
                    // detecting a search call. Close them and fully reset the
                    // tracker so the next loop starts with a clean slate
                    // (ever_opened = false, next_idx = 0).
                    for (_, idx) in tracker.close_all() {
                        let _ = send_sse(&tx, crate::sse::emit_block_stop(idx)).await;
                    }
                    tracker.reset();

                    // Loop again with updated history to fetch search-informed response
                    continue;
                }

                // Flush remaining text and DSML buffers at end of stream
                ctx.flush_remaining(&mut tracker, &tx, &builder, &current_payload)
                    .await;

                // Close any remaining active content blocks
                for (_, idx) in tracker.close_all() {
                    let _ = send_sse(&tx, crate::sse::emit_block_stop(idx)).await;
                }

                if !tracker.has_any_blocks_ever_opened() {
                    finalize_stream(
                        "empty_upstream_stream",
                        &tx,
                        &builder,
                        &mut tracker,
                        &mut ctx,
                    )
                    .await;
                    break;
                }

                let stop_reason = if ctx.has_emitted_tool_use {
                    "tool_use".to_string()
                } else {
                    ctx.final_stop_reason
                };

                // Send final message_delta and message_stop
                let output_tokens = estimate_string_tokens(&ctx.accumulated_thinking)
                    + estimate_string_tokens(&ctx.accumulated_text);
                let output_tokens = if output_tokens == 0 && ctx.has_emitted_tool_use {
                    15
                } else {
                    output_tokens
                };

                let _ = send_sse(
                    &tx,
                    builder.message_delta_with_stop(&stop_reason, output_tokens),
                )
                .await;

                let _ = send_sse(&tx, builder.message_stop()).await;
                break;
            }
        }
    });

    // Wrap the stream so the cancellation token fires when the client drops the receiver.
    let stream = DropCancel {
        token: cancel_token,
        inner: Box::pin(ReceiverStream::new(rx).map(Ok)),
    };

    Ok(stream)
}

fn split_pending_text(text: &str) -> (String, String) {
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

fn get_correct_tool_name(name: &str, payload: &MessagesRequest) -> String {
    if let Some(ref tools) = payload.tools {
        let name_lower = name.to_lowercase();
        for t in tools {
            if t.name.to_lowercase() == name_lower {
                return t.name.clone();
            }
        }
    }
    name.to_string()
}

pub fn estimate_string_tokens(text: &str) -> u32 {
    let mut tokens: f32 = 0.0;
    let mut in_word = false;

    for c in text.chars() {
        if c.is_whitespace() {
            tokens += 0.25;
            in_word = false;
        } else if c.is_ascii_alphanumeric() {
            if !in_word {
                tokens += 1.0;
                in_word = true;
            } else {
                tokens += 0.22;
            }
        } else {
            tokens += 0.5;
            in_word = false;
        }
    }
    tokens.round() as u32
}

pub fn estimate_input_tokens(payload: &MessagesRequest) -> u32 {
    let mut total_tokens = 0;
    if let Some(ref sys) = payload.system {
        total_tokens += estimate_string_tokens(&sys.to_string());
    }
    for msg in &payload.messages {
        match &msg.content {
            ContentVal::Single(text) => total_tokens += estimate_string_tokens(text),
            ContentVal::Multiple(blocks) => {
                for b in blocks {
                    if let Some(ref text) = b.text {
                        total_tokens += estimate_string_tokens(text);
                    }
                    if let Some(ref input) = b.input {
                        total_tokens += estimate_string_tokens(&input.to_string());
                    }
                    if let Some(ref content) = b.content {
                        total_tokens += estimate_string_tokens(&content.to_string());
                    }
                }
            }
        }
    }
    if total_tokens == 0 {
        100
    } else {
        total_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::AnthropicTool;

    #[test]
    fn test_split_pending_text() {
        assert_eq!(
            split_pending_text("hello<"),
            ("hello".to_string(), "<".to_string())
        );
        assert_eq!(
            split_pending_text("hello<｜"),
            ("hello".to_string(), "<｜".to_string())
        );
        assert_eq!(
            split_pending_text("hello<｜DSML｜tool_calls>"),
            ("hello".to_string(), "<｜DSML｜tool_calls>".to_string())
        );
        assert_eq!(
            split_pending_text("hello"),
            ("hello".to_string(), "".to_string())
        );
    }

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
        }
    }

    #[tokio::test]
    async fn test_process_openai_sse_line_keeps_final_partial_delta() {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let builder = SseEventBuilder::new("msg_test".to_string(), "model".to_string());
        let mut tracker = SseBlockTracker::new();
        let mut ctx = StreamContext::new(false);
        ctx.message_started = true;
        let payload = empty_messages_request();
        let line = r#"data: {"choices":[{"delta":{"content":"final words without newline"},"finish_reason":null}]}"#;

        let done =
            process_openai_sse_line(line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

        assert!(!done);
        assert_eq!(ctx.accumulated_text, "final words without newline");
        assert!(tracker.has_any_blocks_ever_opened());
    }

    #[tokio::test]
    async fn test_process_openai_sse_line_emits_thinking_delta() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let builder = SseEventBuilder::new("msg_test".to_string(), "model".to_string());
        let mut tracker = SseBlockTracker::new();
        let mut ctx = StreamContext::new(false);
        ctx.message_started = true;
        let payload = empty_messages_request();
        let line = r#"data: {"choices":[{"delta":{"reasoning_content":"thinking step 1"},"finish_reason":null}]}"#;

        let done =
            process_openai_sse_line(line, &mut ctx, &mut tracker, &tx, &builder, &payload).await;

        assert!(!done);
        assert_eq!(ctx.accumulated_thinking, "thinking step 1");
        assert_eq!(tracker.thinking_idx(), Some(0));
        assert!(tracker.has_any_blocks_ever_opened());

        let start = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for thinking block start")
            .expect("thinking block start event missing");
        let delta = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for thinking delta")
            .expect("thinking delta event missing");

        let start_dbg = format!("{:?}", start);
        let delta_dbg = format!("{:?}", delta);
        assert!(
            start_dbg.contains("content_block_start") && start_dbg.contains("thinking"),
            "expected thinking content_block_start, got: {}",
            start_dbg
        );
        assert!(
            delta_dbg.contains("thinking_delta") && delta_dbg.contains("thinking step 1"),
            "expected thinking_delta event, got: {}",
            delta_dbg
        );
    }

    #[tokio::test]
    async fn test_process_openai_sse_line_streams_thinking_then_text() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let builder = SseEventBuilder::new("msg_test".to_string(), "model".to_string());
        let mut tracker = SseBlockTracker::new();
        let mut ctx = StreamContext::new(false);
        ctx.message_started = true;
        let payload = empty_messages_request();

        let reasoning_line =
            r#"data: {"choices":[{"delta":{"reasoning_content":"think"},"finish_reason":null}]}"#;
        let text_line =
            r#"data: {"choices":[{"delta":{"content":"answer"},"finish_reason":null}]}"#;

        assert!(
            !process_openai_sse_line(
                reasoning_line,
                &mut ctx,
                &mut tracker,
                &tx,
                &builder,
                &payload,
            )
            .await
        );
        assert!(
            !process_openai_sse_line(text_line, &mut ctx, &mut tracker, &tx, &builder, &payload)
                .await
        );

        let mut events = Vec::new();
        for _ in 0..5 {
            events.push(
                tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
                    .await
                    .expect("timed out waiting for SSE event")
                    .expect("SSE event missing"),
            );
        }
        let joined = events
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
            .join("\n---\n");

        assert!(joined.contains("content_block_start") && joined.contains("thinking"));
        assert!(joined.contains("thinking_delta") && joined.contains("think"));
        assert!(joined.contains("content_block_stop"), "events: {}", joined);
        assert!(joined.contains("content_block_start") && joined.contains("text"));
        assert!(joined.contains("text_delta") && joined.contains("answer"));
        assert_eq!(ctx.accumulated_thinking, "think");
        assert_eq!(ctx.accumulated_text, "answer");
    }

    #[tokio::test]
    async fn test_process_openai_sse_line_accepts_reasoning_aliases() {
        for (field, expected) in [("reasoning", "alias-think"), ("thinking", "alias-think-2")] {
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            let builder = SseEventBuilder::new("msg_test".to_string(), "model".to_string());
            let mut tracker = SseBlockTracker::new();
            let mut ctx = StreamContext::new(false);
            ctx.message_started = true;
            let payload = empty_messages_request();
            let line = format!(
                r#"data: {{"choices":[{{"delta":{{"{}":"{}"}},"finish_reason":null}}]}}"#,
                field, expected
            );

            let done =
                process_openai_sse_line(&line, &mut ctx, &mut tracker, &tx, &builder, &payload)
                    .await;

            assert!(!done);
            let start = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
                .await
                .expect("timed out waiting for thinking block start")
                .expect("thinking block start event missing");
            let delta = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
                .await
                .expect("timed out waiting for thinking delta")
                .expect("thinking delta event missing");
            let joined = format!("{:?}\n{:?}", start, delta);
            assert!(joined.contains("thinking"), "events: {}", joined);
            assert!(joined.contains("thinking_delta"), "events: {}", joined);
            assert!(joined.contains(expected), "events: {}", joined);
        }
    }

    #[tokio::test]
    async fn test_process_openai_sse_line_done_marker_stops_outer_stream() {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let builder = SseEventBuilder::new("msg_test".to_string(), "model".to_string());
        let mut tracker = SseBlockTracker::new();
        let mut ctx = StreamContext::new(false);
        let payload = empty_messages_request();

        let done = process_openai_sse_line(
            "data: [DONE]",
            &mut ctx,
            &mut tracker,
            &tx,
            &builder,
            &payload,
        )
        .await;

        assert!(done);
    }

    #[test]
    fn test_get_correct_tool_name() {
        let req = MessagesRequest {
            model: Some("model".to_string()),
            messages: vec![],
            system: None,
            tools: Some(vec![AnthropicTool {
                name: "Skill".to_string(),
                description: "Skill tool".to_string(),
                input_schema: serde_json::json!({}),
            }]),
            tool_choice: None,
            stream: false,
            temperature: None,
            max_tokens: Some(100),
        };
        assert_eq!(get_correct_tool_name("skill", &req), "Skill");
        assert_eq!(get_correct_tool_name("Skill", &req), "Skill");
        assert_eq!(get_correct_tool_name("other", &req), "other");
    }
}
