//! Streaming request execution loop and search-interception retries.

use super::context::{finalize_stream, process_openai_sse_line, StreamContext};
use super::transport::{send_sse, DropCancel};
use crate::error::BridgeError;
use crate::handlers::MessagesRequest;
use crate::opencode::forward::common::{
    estimate_input_tokens, estimate_string_tokens, inject_search_results,
};
use crate::opencode::mapper::{
    extract_search_query, is_compact_request, map_anthropic_to_openai_with_policy,
};
use crate::opencode::retry::execute_with_warp_retry;
use crate::opencode::search::SearchClient;
use crate::sse::SseEventBuilder;
use crate::state::AppState;
use crate::stream_tracker::SseBlockTracker;
use axum::response::sse::Event;
use futures_util::{Stream, StreamExt};
use std::convert::Infallible;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info};

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
    // Child token is cancelled both by client stream drop and by global server shutdown.
    let cancel_token = state.workers.cancellation_token();
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

    let workers = state_clone.workers.clone();
    workers.spawn_ephemeral("llm-stream", {
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
                    let failure = crate::opencode::retry::cancellation_failure();
                    info!(?failure, "client disconnected; cancelling streaming task");
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

                let openai_req = map_anthropic_to_openai_with_policy(
                    &current_payload,
                    model_clone.clone(),
                    state_clone.config.protocol.min_reasoning_stream_tokens,
                );

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
