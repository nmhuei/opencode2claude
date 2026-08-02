//! Streaming request execution loop and search-interception retries.

use super::context::{
    finalize_stream, finalize_stream_with_text, process_openai_sse_line, StreamContext,
};
use super::transport::{send_sse, DropCancel};
use crate::error::BridgeError;
use crate::handlers::MessagesRequest;
use crate::history::HistoryCapture;
use crate::opencode::forward::common::{
    estimate_input_tokens, estimate_string_tokens, inject_search_results, normalize_search_query,
    prepare_compat_tool_retry, prepare_final_search_synthesis, resolve_search_query,
    search_results_with_instruction,
};
use crate::opencode::mapper::{is_compact_request, map_anthropic_to_openai_with_policy};
use crate::opencode::retry::execute_with_warp_retry;
use crate::opencode::search::SearchClient;
use crate::sse::SseEventBuilder;
use crate::state::AppState;
use crate::stream_tracker::SseBlockTracker;
use axum::response::sse::Event;
use bytes::BytesMut;
use futures_util::{Stream, StreamExt};
use memchr::memchr;
use std::collections::HashMap;
use std::convert::Infallible;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info, trace};

const MAX_COMPAT_TOOL_RETRIES: u32 = 2;

/// Remove one complete logical SSE line without copying the unread tail.
///
/// `BytesMut::split_to` is O(1), so this avoids the repeated allocation and
/// left-shift caused by `Vec::drain(..line_len).collect()` on busy streams.
fn take_next_sse_line(
    buffer: &mut BytesMut,
    max_line_bytes: usize,
) -> Result<Option<BytesMut>, usize> {
    match memchr(b'\n', buffer.as_ref()) {
        Some(position) => {
            let line_len = position + 1;
            if line_len > max_line_bytes {
                return Err(line_len);
            }
            Ok(Some(buffer.split_to(line_len)))
        }
        None if buffer.len() > max_line_bytes => Err(buffer.len()),
        None => Ok(None),
    }
}

/// Perform a streaming completions request to upstream OpenCode API and stream Anthropic SSE chunks.
#[allow(clippy::too_many_arguments)]
pub async fn forward_to_llm_stream(
    state: &AppState,
    api_key: String,
    payload: MessagesRequest,
    model: String,
    channel_capacity: usize,
    search_client: SearchClient,
    max_search_loops: u32,
    capture: HistoryCapture,
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
            let mut stream_metrics = state_clone.metrics.begin_stream();
            let mut current_payload = payload;
            let mut upstream_turns = 0_u32;
            let mut completed_searches = 0_u32;
            let mut search_cache = HashMap::<String, String>::new();
            let mut synthesis_only = false;
            let mut compat_tool_retries = 0_u32;
            let mut message_emitted = false;
            let mut tracker = SseBlockTracker::new();

            loop {
                // Check if client disconnected
                if cancel_token_spawn.is_cancelled() {
                    let failure = crate::opencode::retry::cancellation_failure();
                    stream_metrics.cancelled();
                    capture.cancel();
                    info!(?failure, "client disconnected; cancelling streaming task");
                    break;
                }

                upstream_turns = upstream_turns.saturating_add(1);
                let is_compact = is_compact_request(&current_payload);
                if upstream_turns
                    > max_search_loops
                        .saturating_add(MAX_COMPAT_TOOL_RETRIES)
                        .saturating_add(3)
                {
                    error!(
                        upstream_turns,
                        max_search_loops,
                        "Search synthesis turn limit reached"
                    );
                    let terminal = "Web research reached the configured turn limit. Additional searches were suppressed; use the results already collected in this conversation.";
                    finalize_stream_with_text(
                        terminal,
                        &tx,
                        &builder,
                        &mut tracker,
                        message_emitted,
                    )
                    .await;
                    capture.append_response(terminal);
                    capture.finish_success(200, Some("end_turn"), Some(&model_clone));
                    stream_metrics.completed();
                    break;
                }

                let openai_req = map_anthropic_to_openai_with_policy(
                    &current_payload,
                    model_clone.clone(),
                    state_clone.config.protocol.min_reasoning_stream_tokens,
                );
                if let Ok(value) = serde_json::to_value(&openai_req) {
                    capture.effective_json(
                        &value,
                        Some(&openai_req.model),
                        "primary",
                        upstream_turns,
                    );
                }

                info!(
                    "Forwarding stream request for model {} (turn {})",
                    model_clone, upstream_turns
                );

                let res = match execute_with_warp_retry(&state_clone, &api_key_clone, &openai_req)
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        error!("Error forwarding upstream request: {}", e);
                        capture.attempt_finished(
                            None,
                            "failed",
                            None,
                            Some("transport_or_provider_error"),
                            Some(&e.to_string()),
                        );
                        capture.fail(None, "forward_error", &e.to_string());
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
                    capture.provider_raw_response(&body);
                    capture.attempt_finished(
                        Some(status.as_u16()),
                        "failed",
                        None,
                        Some("upstream_non_2xx"),
                        Some(&format!("upstream returned status {status}")),
                    );
                    capture.fail(
                        Some(status.as_u16()),
                        "upstream_non_2xx",
                        &format!("upstream returned status {status}"),
                    );
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
                let max_sse_line_bytes = state_clone.config.protocol.max_sse_line_bytes;
                let mut line_buffer = BytesMut::with_capacity(
                    state_clone
                        .config
                        .stream_buffer_size
                        .min(max_sse_line_bytes),
                );
                let mut stream_done = false;
                let mut client_cancelled = false;
                let mut first_chunk_recorded = false;

                let mut ctx = StreamContext::new(is_compact);

                if upstream_turns == 1 {
                    let input_tokens = estimate_input_tokens(&current_payload);
                    let _ = send_sse(&tx, builder.message_start(input_tokens)).await;
                    ctx.message_started = true;
                    message_emitted = true;
                } else {
                    // On search intercept turns, message_start was already
                    // emitted in the first iteration
                    ctx.message_started = true;
                }

                loop {
                    let next_chunk = tokio::select! {
                        biased;
                        _ = cancel_token_spawn.cancelled() => {
                            client_cancelled = true;
                            None
                        }
                        chunk = bytes_stream.next() => chunk,
                    };
                    let Some(chunk_res) = next_chunk else {
                        break;
                    };
                    let chunk = match chunk_res {
                        Ok(c) => c,
                        Err(e) => {
                            error!("Error reading chunk from upstream: {}", e);
                            ctx.stream_failed = true;
                            break;
                        }
                    };
                    if !first_chunk_recorded {
                        capture.first_chunk();
                        first_chunk_recorded = true;
                    }
                    trace!(
                        upstream_turn = upstream_turns,
                        bytes = chunk.len(),
                        "stream_timing upstream_sse_bytes_received"
                    );
                    line_buffer.extend_from_slice(&chunk);
                    loop {
                        let line_bytes = match take_next_sse_line(
                            &mut line_buffer,
                            max_sse_line_bytes,
                        ) {
                            Ok(Some(line)) => line,
                            Ok(None) => break,
                            Err(current_bytes) => {
                                error!(
                                    current_bytes,
                                    max_bytes = max_sse_line_bytes,
                                    "Upstream SSE line exceeded configured byte limit"
                                );
                                let error_ev = Event::default()
                                    .event("error")
                                    .json_data(serde_json::json!({
                                        "type": "error",
                                        "error": {
                                            "type": "api_error",
                                            "message": "Upstream SSE line exceeded configured byte limit"
                                        }
                                    }))
                                    .unwrap_or_else(|_| Event::default().data("{}"));
                                let _ = send_sse(&tx, error_ev).await;
                                ctx.stream_failed = true;
                                stream_done = true;
                                break;
                            }
                        };
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

                if client_cancelled {
                    capture.append_reasoning(&ctx.accumulated_thinking);
                    capture.append_response(&ctx.accumulated_text);
                    capture.attempt_finished(
                        None,
                        "cancelled",
                        None,
                        Some("client_cancelled"),
                        Some("client disconnected"),
                    );
                    capture.cancel();
                    stream_metrics.cancelled();
                    info!("client disconnected; upstream stream dropped immediately");
                    break;
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
                    capture.append_reasoning(&ctx.accumulated_thinking);
                    capture.append_response(&ctx.accumulated_text);
                    capture.attempt_finished(
                        None,
                        "failed",
                        None,
                        Some("upstream_read_error"),
                        Some("upstream stream ended with a read error"),
                    );
                    capture.fail(
                        None,
                        "upstream_read_error",
                        "upstream stream ended with a read error",
                    );
                    finalize_stream("upstream_read_error", &tx, &builder, &mut tracker, &mut ctx)
                        .await;
                    break;
                }

                // Finalize retained text/DSML/compat/native tool buffers before
                // deciding whether this turn is a search interception, retry,
                // regular tool_use response, or final text response.
                ctx.flush_remaining(&mut tracker, &tx, &builder, &current_payload)
                    .await;

                if ctx.intercepting_search && !ctx.compat_retry_requested {
                    capture.append_reasoning(&ctx.accumulated_thinking);
                    capture.tool_call(&ctx.search_tc_name, Some(&ctx.search_tc_args));
                    let input_val: serde_json::Value = serde_json::from_str(&ctx.search_tc_args)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    let (search_query, used_fallback) =
                        resolve_search_query(&ctx.search_tc_args, &current_payload);
                    let normalized_query = normalize_search_query(&search_query);
                    let cached = search_cache.get(&normalized_query).cloned();
                    let duplicate_query = cached.is_some();
                    let budget_exhausted = completed_searches >= max_search_loops;

                    info!(
                        query = %search_query,
                        used_fallback,
                        completed_searches,
                        max_search_loops,
                        duplicate = duplicate_query,
                        "Intercepted stream search tool call"
                    );

                    if synthesis_only {
                        let collected = search_cache
                            .values()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n\n")
                            .chars()
                            .take(6000)
                            .collect::<String>();
                        let terminal = if collected.trim().is_empty() {
                            "Web research is complete, but the model requested another search. Additional searches were suppressed and no usable result context was available."
                                .to_string()
                        } else {
                            format!(
                                "Web research reached the configured search budget. Additional searches were suppressed. Available sourced results:\n\n{collected}"
                            )
                        };
                        finalize_stream_with_text(
                            &terminal,
                            &tx,
                            &builder,
                            &mut tracker,
                            message_emitted,
                        )
                        .await;
                        capture.append_response(&terminal);
                        capture.attempt_finished(
                            Some(200),
                            "completed",
                            Some("end_turn"),
                            None,
                            None,
                        );
                        capture.finish_success(200, Some("end_turn"), Some(&model_clone));
                        stream_metrics.completed();
                        break;
                    }

                    let search_results = if let Some(results) = cached {
                        results
                    } else if budget_exhausted {
                        "Web search budget reached. No additional network search was executed."
                            .to_string()
                    } else {
                        let results = search_client.search(&search_query).await;
                        completed_searches = completed_searches.saturating_add(1);
                        search_cache.insert(normalized_query, results.clone());
                        info!("Search completed. Results length: {}", results.len());
                        results
                    };

                    let should_finalize =
                        duplicate_query || budget_exhausted || completed_searches >= max_search_loops;
                    let injected_results =
                        search_results_with_instruction(&search_results, should_finalize);
                    inject_search_results(
                        &mut current_payload,
                        &injected_results,
                        &ctx.accumulated_thinking,
                        &ctx.accumulated_text,
                        &ctx.search_tc_id,
                        &ctx.search_tc_name,
                        &input_val,
                    );
                    capture.search(&search_query, Some(&search_results));
                    capture.attempt_finished(
                        Some(200),
                        "completed",
                        Some("tool_calls"),
                        None,
                        None,
                    );
                    if should_finalize {
                        prepare_final_search_synthesis(
                            &mut current_payload,
                            if duplicate_query {
                                "duplicate search query"
                            } else {
                                "configured search budget reached"
                            },
                        );
                        synthesis_only = true;
                    }

                    for (_, idx) in tracker.close_all() {
                        let _ = send_sse(&tx, crate::sse::emit_block_stop(idx)).await;
                    }
                    tracker.reset();
                    continue;
                }

                if ctx.compat_retry_requested {
                    if compat_tool_retries < MAX_COMPAT_TOOL_RETRIES {
                        compat_tool_retries = compat_tool_retries.saturating_add(1);
                        info!(
                            retry = compat_tool_retries,
                            max_retries = MAX_COMPAT_TOOL_RETRIES,
                            "Retrying upstream after malformed compatibility tool marker"
                        );
                        capture.attempt_finished(
                            Some(200),
                            "retrying",
                            Some("malformed_tool_marker"),
                            Some("malformed_tool_marker"),
                            Some("model emitted an unsupported or incomplete tool marker"),
                        );
                        prepare_compat_tool_retry(&mut current_payload);
                        for (_, idx) in tracker.close_all() {
                            let _ = send_sse(&tx, crate::sse::emit_block_stop(idx)).await;
                        }
                        tracker.reset();
                        continue;
                    }

                    let terminal = "The upstream model repeatedly emitted an incomplete tool request. No unsafe or partial tool call was executed.";
                    finalize_stream_with_text(
                        terminal,
                        &tx,
                        &builder,
                        &mut tracker,
                        message_emitted,
                    )
                    .await;
                    capture.append_response(terminal);
                    capture.attempt_finished(
                        Some(200),
                        "failed",
                        Some("malformed_tool_marker"),
                        Some("malformed_tool_marker"),
                        Some("compatibility tool retry budget exhausted"),
                    );
                    capture.fail(
                        Some(200),
                        "malformed_tool_marker",
                        "compatibility tool retry budget exhausted",
                    );
                    stream_metrics.completed();
                    break;
                }

                // Close any remaining active content blocks
                for (_, idx) in tracker.close_all() {
                    let _ = send_sse(&tx, crate::sse::emit_block_stop(idx)).await;
                }

                if !tracker.has_any_blocks_ever_opened() {
                    capture.append_reasoning(&ctx.accumulated_thinking);
                    capture.append_response(&ctx.accumulated_text);
                    capture.attempt_finished(
                        Some(200),
                        "failed",
                        Some("empty_upstream_stream"),
                        Some("empty_upstream_stream"),
                        Some("upstream stream produced no content blocks"),
                    );
                    capture.fail(
                        Some(200),
                        "empty_upstream_stream",
                        "upstream stream produced no content blocks",
                    );
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
                capture.append_reasoning(&ctx.accumulated_thinking);
                capture.append_response(&ctx.accumulated_text);
                if ctx.has_emitted_tool_use {
                    capture.tool_call("tool_use", None);
                }
                capture.usage(
                    Some(u64::from(estimate_input_tokens(&current_payload))),
                    Some(u64::from(output_tokens)),
                    Some(u64::from(estimate_string_tokens(&ctx.accumulated_thinking))),
                );
                capture.attempt_finished(
                    Some(200),
                    "completed",
                    Some(&stop_reason),
                    None,
                    None,
                );
                capture.finish_success(200, Some(&stop_reason), Some(&model_clone));
                stream_metrics.completed();
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

#[cfg(test)]
mod line_buffer_tests {
    use super::*;

    #[test]
    fn extracts_complete_lines_without_consuming_pending_tail() {
        let mut buffer = BytesMut::from(&b"one\ntwo\npartial"[..]);

        let first = take_next_sse_line(&mut buffer, 32).unwrap().unwrap();
        let second = take_next_sse_line(&mut buffer, 32).unwrap().unwrap();

        assert_eq!(first.as_ref(), b"one\n");
        assert_eq!(second.as_ref(), b"two\n");
        assert_eq!(buffer.as_ref(), b"partial");
        assert!(take_next_sse_line(&mut buffer, 32).unwrap().is_none());
    }

    #[test]
    fn aggregate_chunk_may_exceed_limit_when_each_line_is_valid() {
        let mut buffer = BytesMut::from(&b"a\nb\nc\nd\n"[..]);
        let mut lines = Vec::new();

        while let Some(line) = take_next_sse_line(&mut buffer, 2).unwrap() {
            lines.push(line);
        }

        assert_eq!(lines.len(), 4);
        assert!(buffer.is_empty());
    }

    #[test]
    fn oversized_logical_line_is_rejected_across_chunk_boundaries() {
        let mut buffer = BytesMut::from(&b"abc"[..]);
        assert!(take_next_sse_line(&mut buffer, 4).unwrap().is_none());

        buffer.extend_from_slice(b"de\n");
        assert_eq!(take_next_sse_line(&mut buffer, 4), Err(6));
    }
}
