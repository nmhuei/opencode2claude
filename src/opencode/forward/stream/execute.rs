//! Streaming request execution loop and search-interception retries.

use super::context::{
    finalize_stream, finalize_stream_with_text, process_openai_sse_line, send_tracked,
    StreamContext,
};
use super::transport::{send_sse, DropCancel};
use crate::error::BridgeError;
use crate::handlers::MessagesRequest;
use crate::history::HistoryCapture;
use crate::observability::{StreamMetricsGuard, ToolProtocolMetricClass};
use crate::opencode::forward::common::{
    estimate_input_tokens, estimate_string_tokens, inject_search_results, normalize_search_query,
    prepare_compat_tool_retry, prepare_final_search_synthesis, prepare_native_tool_retry,
    resolve_search_query, search_results_with_instruction,
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
const MAX_ENCODED_NATIVE_RETRIES: u32 = 1;
const MAX_STREAM_READ_RETRIES: u32 = 2;

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

/// Close a failed turn with one complete error lifecycle: message_start (unless
/// a previous search-loop iteration already started it) and a single error
/// event. Per the Messages API spec an error event ends the stream — no
/// message_delta or message_stop may follow it.
async fn finalize_transport_error(
    message: &str,
    tx: &tokio::sync::mpsc::Sender<Event>,
    builder: &SseEventBuilder,
    message_started: bool,
) {
    if !message_started {
        let _ = send_sse(tx, builder.message_start(0)).await;
    }
    let _ = send_sse(tx, builder.api_error(message)).await;
}

/// Record bookkeeping for a consumer that stopped accepting SSE events
/// (receiver dropped or stalled past the bounded send window) and report
/// whether the task must terminate now. The response ends without any clean
/// terminator: half-open blocks stay half-open because the connection is gone.
fn dead_consumer_takedown(
    ctx: &StreamContext,
    capture: &HistoryCapture,
    stream_metrics: &mut StreamMetricsGuard,
) -> bool {
    if !ctx.send_failed {
        return false;
    }
    capture.append_reasoning(&ctx.accumulated_thinking);
    capture.append_response(&ctx.accumulated_text);
    capture.attempt_finished(
        None,
        "cancelled",
        None,
        Some("sse_send_failed"),
        Some("SSE consumer stopped accepting events"),
    );
    capture.cancel();
    stream_metrics.cancelled();
    info!("SSE consumer stopped accepting events; stream terminated without a clean end");
    true
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
            let mut encoded_native_retries = 0_u32;
            let mut stream_read_retries = 0_u32;
            let mut message_emitted = false;
            let mut tracker = SseBlockTracker::new();

            'turns: loop {
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
                // Per-attempt baseline for retry gates: block indices are
                // monotonic within one message (never reset), so the gates
                // below compare this attempt's allocations instead of the
                // turn-global ever_opened flag, which earlier search or
                // interception rounds may have set.
                let attempt_start_allocated = tracker.allocated_blocks();
                if upstream_turns
                    > max_search_loops
                        .saturating_add(MAX_COMPAT_TOOL_RETRIES)
                        .saturating_add(MAX_ENCODED_NATIVE_RETRIES)
                        .saturating_add(MAX_STREAM_READ_RETRIES)
                        .saturating_add(3)
                {
                    error!(
                        upstream_turns,
                        max_search_loops,
                        "Search synthesis turn limit reached"
                    );
                    let terminal = "Web research reached the configured turn limit. Additional searches were suppressed; use the results already collected in this conversation.";
                    // This gate fires before the per-attempt context exists;
                    // build a throwaway one carrying the message lifecycle so
                    // the finalizer neither re-emits nor skips message_start.
                    let mut ctx = StreamContext::new_with_encoded_fallback(is_compact, false);
                    ctx.message_started = message_emitted;
                    finalize_stream_with_text(
                        terminal,
                        &tx,
                        &builder,
                        &mut tracker,
                        &mut ctx,
                    )
                    .await;
                    if dead_consumer_takedown(&ctx, &capture, &mut stream_metrics) {
                        break 'turns;
                    }
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
                        // Emit one complete error turn: message_start (unless a
                        // previous search-loop iteration already started one),
                        // a single error event, then message_stop. Sending the
                        // error before message_start, or letting finalize_stream
                        // re-emit a second error, violates the SSE ordering.
                        finalize_transport_error(
                            &format!("Bridge upstream error: {}", e),
                            &tx,
                            &builder,
                            message_emitted,
                        )
                        .await;
                        break;
                    }
                };
                capture.attempt_route(res.route());

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
                    // Error event carries the status only (no body leak to client).
                    finalize_transport_error(
                        &format!("Upstream returned {}", status),
                        &tx,
                        &builder,
                        message_emitted,
                    )
                    .await;
                    break;
                }

                let response_proxy_index = res.proxy_index();
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

                let mut ctx = StreamContext::new_with_encoded_fallback(
                    is_compact,
                    encoded_native_retries > 0,
                );

                if upstream_turns == 1 {
                    let input_tokens = estimate_input_tokens(&current_payload);
                    send_tracked(&mut ctx, &tx, builder.message_start(input_tokens)).await;
                    if dead_consumer_takedown(&ctx, &capture, &mut stream_metrics) {
                        break 'turns;
                    }
                    ctx.message_started = true;
                    message_emitted = true;
                } else {
                    // On search intercept turns, message_start was already
                    // emitted in the first iteration
                    ctx.message_started = true;
                }

                let mut line_limit_exceeded = false;

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
                            if let Some(index) = response_proxy_index {
                                state_clone.proxy_pool.write().await.record_failure(index);
                            }
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
                                send_tracked(
                                    &mut ctx,
                                    &tx,
                                    builder.api_error(
                                        "Upstream SSE line exceeded configured byte limit",
                                    ),
                                )
                                .await;
                                ctx.stream_failed = true;
                                ctx.error_terminated = true;
                                line_limit_exceeded = true;
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

                // A failed SSE send means the client channel is dead or stalled
                // past the bounded window. Terminate exactly like a client
                // cancellation: no finalize, no fake message_delta/message_stop,
                // nothing further emitted. Half-open blocks intentionally stay
                // half-open — the connection is gone.
                if dead_consumer_takedown(&ctx, &capture, &mut stream_metrics) {
                    break 'turns;
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

                // An error event is terminal per the Messages spec: once emitted,
                // the stream must end at it — no message_delta, message_stop, or
                // retry/continuation may follow, or the receiving SDK treats the
                // turn as truncated mid-render. Break immediately and record the
                // failure instead of replaying or finalizing a clean end_turn.
                if ctx.error_terminated {
                    capture.append_reasoning(&ctx.accumulated_thinking);
                    capture.append_response(&ctx.accumulated_text);
                    capture.attempt_finished(
                        None,
                        "failed",
                        None,
                        Some("mid_stream_upstream_error"),
                        Some("upstream ended the stream with an error event"),
                    );
                    capture.fail(
                        None,
                        "mid_stream_upstream_error",
                        "upstream ended the stream with an error event",
                    );
                    stream_metrics.failed();
                    info!(
                        "mid-stream upstream error event; stream ended at the error (no message_delta/message_stop)"
                    );
                    break;
                }

                if ctx.stream_failed {
                    capture.append_reasoning(&ctx.accumulated_thinking);
                    capture.append_response(&ctx.accumulated_text);
                    capture.attempt_finished(
                        None,
                        "failed",
                        None,
                        Some("upstream_read_error"),
                        Some("upstream stream ended with a read error"),
                    );

                    // Retrying after any content block was emitted can duplicate
                    // visible text or execute a tool twice. When nothing was
                    // emitted this attempt, however, only message_start reached
                    // the client, so the same request can be replayed safely
                    // while preserving one Anthropic message lifecycle. A
                    // line-limit failure is never retryable: the client already
                    // received the SSE error event, so a replay would orphan
                    // content after a terminal error.
                    if !line_limit_exceeded
                        && tracker.allocated_blocks() == attempt_start_allocated
                        && stream_read_retries < MAX_STREAM_READ_RETRIES
                    {
                        stream_read_retries = stream_read_retries.saturating_add(1);
                        info!(
                            retry = stream_read_retries,
                            max_retries = MAX_STREAM_READ_RETRIES,
                            "retrying upstream stream after pre-content read failure"
                        );
                        // No reset(): block indices stay monotonic within the
                        // message, even when earlier search rounds allocated
                        // blocks.
                        continue;
                    }

                    error!("Stream failed after visible output or retry exhaustion — finalizing stream");
                    capture.fail(
                        None,
                        "upstream_read_error",
                        "upstream stream ended with a read error",
                    );
                    finalize_stream("upstream_read_error", &tx, &builder, &mut tracker, &mut ctx)
                        .await;
                    break;
                }

                stream_read_retries = 0;

                // Finalize retained text/DSML/compat/native tool buffers before
                // deciding whether this turn is a search interception, retry,
                // regular tool_use response, or final text response.
                ctx.flush_remaining(&mut tracker, &tx, &builder, &current_payload)
                    .await;

                if ctx.encoded_candidate_seen {
                    state_clone.metrics.record_tool_protocol(
                        ToolProtocolMetricClass::EncodedCandidate,
                        1,
                    );
                    capture.tool_protocol("encoded_candidate", "encoded", 1, None);
                }
                if ctx.literal_marker_suppressed {
                    state_clone.metrics.record_tool_protocol(
                        ToolProtocolMetricClass::LiteralMarkerSuppression,
                        1,
                    );
                    capture.tool_protocol(
                        "literal_marker_suppressed",
                        "encoded",
                        1,
                        Some("explicit literal/meta-output user intent"),
                    );
                }
                if ctx.native_tool_calls_emitted > 0 {
                    let count = u64::from(ctx.native_tool_calls_emitted);
                    state_clone
                        .metrics
                        .record_tool_protocol(ToolProtocolMetricClass::NativeToolCall, count);
                    capture.tool_protocol("tool_calls", "native", count, None);
                }
                if ctx.encoded_tool_calls_emitted > 0 {
                    let count = u64::from(ctx.encoded_tool_calls_emitted);
                    state_clone.metrics.record_tool_protocol(
                        ToolProtocolMetricClass::EncodedFallbackToolCall,
                        count,
                    );
                    capture.tool_protocol("tool_calls", "encoded_fallback", count, None);
                }

                if ctx.native_recovery_retry_requested {
                    if tracker.allocated_blocks() == attempt_start_allocated
                        && !ctx.has_emitted_tool_use
                        && encoded_native_retries < MAX_ENCODED_NATIVE_RETRIES
                    {
                        encoded_native_retries = encoded_native_retries.saturating_add(1);
                        state_clone.metrics.record_tool_protocol(
                            ToolProtocolMetricClass::EncodedNativeRetry,
                            1,
                        );
                        capture.tool_protocol(
                            "native_retry",
                            "encoded_recovery",
                            1,
                            Some("retry encoded candidate through native protocol"),
                        );
                        info!(
                            retry = encoded_native_retries,
                            max_retries = MAX_ENCODED_NATIVE_RETRIES,
                            "Retrying encoded tool candidate through native tool protocol"
                        );
                        capture.attempt_finished(
                            Some(200),
                            "retrying",
                            Some("encoded_native_recovery"),
                            Some("encoded_native_recovery"),
                            Some("encoded tool candidate will be retried using native tool protocol"),
                        );
                        prepare_native_tool_retry(&mut current_payload);
                        for (_, idx) in tracker.close_all() {
                            send_tracked(&mut ctx, &tx, crate::sse::emit_block_stop(idx)).await;
                        }
                        if dead_consumer_takedown(&ctx, &capture, &mut stream_metrics) {
                            break 'turns;
                        }
                        continue;
                    }

                    let terminal = "The upstream model emitted an encoded tool request that could not be safely retried through the native tool protocol. No encoded tool call was executed.";
                    finalize_stream_with_text(
                        terminal,
                        &tx,
                        &builder,
                        &mut tracker,
                        &mut ctx,
                    )
                    .await;
                    if dead_consumer_takedown(&ctx, &capture, &mut stream_metrics) {
                        break 'turns;
                    }
                    capture.append_response(terminal);
                    capture.attempt_finished(
                        Some(200),
                        "failed",
                        Some("encoded_native_recovery"),
                        Some("encoded_native_recovery"),
                        Some("native recovery retry was unsafe or exhausted"),
                    );
                    capture.fail(
                        Some(200),
                        "encoded_native_recovery",
                        "native recovery retry was unsafe or exhausted",
                    );
                    stream_metrics.completed();
                    break;
                }

                if ctx.encoded_fallback_rejected {
                    state_clone.metrics.record_tool_protocol(
                        ToolProtocolMetricClass::EncodedFallbackRejection,
                        1,
                    );
                    capture.tool_protocol(
                        "encoded_rejection",
                        "encoded",
                        1,
                        Some("encoded marker named an unavailable tool"),
                    );
                    let terminal = "The upstream model emitted an encoded request for a tool that is not safely available in this request. No tool call was executed.";
                    finalize_stream_with_text(
                        terminal,
                        &tx,
                        &builder,
                        &mut tracker,
                        &mut ctx,
                    )
                    .await;
                    if dead_consumer_takedown(&ctx, &capture, &mut stream_metrics) {
                        break 'turns;
                    }
                    capture.append_response(terminal);
                    capture.attempt_finished(
                        Some(200),
                        "failed",
                        Some("encoded_fallback_rejected"),
                        Some("encoded_fallback_rejected"),
                        Some("encoded fallback candidate failed the safety gate"),
                    );
                    capture.fail(
                        Some(200),
                        "encoded_fallback_rejected",
                        "encoded fallback candidate failed the safety gate",
                    );
                    stream_metrics.completed();
                    break;
                }

                if ctx.intercepting_search && !ctx.compat_retry_requested {
                    capture.append_reasoning(&ctx.accumulated_thinking);
                    // Visible text emitted before the marker belongs in the
                    // response transcript even though this turn ends in
                    // interception.
                    capture.append_response(&ctx.accumulated_text);
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
                            &mut ctx,
                        )
                        .await;
                        if dead_consumer_takedown(&ctx, &capture, &mut stream_metrics) {
                            break 'turns;
                        }
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
                        send_tracked(&mut ctx, &tx, crate::sse::emit_block_stop(idx)).await;
                    }
                    if dead_consumer_takedown(&ctx, &capture, &mut stream_metrics) {
                        break 'turns;
                    }
                    // Intentionally no reset(): block indices must stay monotonic
                    // within one Anthropic message across search loop iterations,
                    // or the client overwrites earlier blocks by reused index.
                    continue;
                }

                if ctx.compat_retry_requested {
                    // Retrying after any content block was emitted this attempt
                    // would append the retried stream's text to blocks the
                    // client already received, merging two upstream responses
                    // into one message (visible duplicate content). Only replay
                    // when nothing content-visible reached the client in this
                    // attempt; blocks from earlier search/interception rounds
                    // do not make the replay unsafe.
                    if tracker.allocated_blocks() == attempt_start_allocated
                        && compat_tool_retries < MAX_COMPAT_TOOL_RETRIES
                    {
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
                            send_tracked(&mut ctx, &tx, crate::sse::emit_block_stop(idx)).await;
                        }
                        if dead_consumer_takedown(&ctx, &capture, &mut stream_metrics) {
                            break 'turns;
                        }
                        // No reset(): keep block indices monotonic within the
                        // message across the retried upstream stream.
                        continue;
                    }

                    let terminal = "The upstream model repeatedly emitted an incomplete tool request. No unsafe or partial tool call was executed.";
                    finalize_stream_with_text(
                        terminal,
                        &tx,
                        &builder,
                        &mut tracker,
                        &mut ctx,
                    )
                    .await;
                    if dead_consumer_takedown(&ctx, &capture, &mut stream_metrics) {
                        break 'turns;
                    }
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
                    send_tracked(&mut ctx, &tx, crate::sse::emit_block_stop(idx)).await;
                }
                if dead_consumer_takedown(&ctx, &capture, &mut stream_metrics) {
                    break 'turns;
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
                    // Cloned, not moved: the tracked terminator sends below
                    // still need mutable access to the context.
                    ctx.final_stop_reason.clone()
                };

                // Send final message_delta and message_stop
                let output_tokens = estimate_string_tokens(&ctx.accumulated_thinking)
                    + estimate_string_tokens(&ctx.accumulated_text);
                let output_tokens = if output_tokens == 0 && ctx.has_emitted_tool_use {
                    15
                } else {
                    output_tokens
                };

                send_tracked(
                    &mut ctx,
                    &tx,
                    builder.message_delta_with_stop(&stop_reason, output_tokens),
                )
                .await;

                send_tracked(&mut ctx, &tx, builder.message_stop()).await;
                if dead_consumer_takedown(&ctx, &capture, &mut stream_metrics) {
                    break 'turns;
                }
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

    #[tokio::test]
    async fn transport_error_ends_stream_at_error_event_without_message_stop() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let builder = SseEventBuilder::new("msg_error".to_string(), "model".to_string());

        finalize_transport_error("boom", &tx, &builder, true).await;

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(format!("{event:?}"));
        }
        assert_eq!(
            events.len(),
            1,
            "an error event must end the stream; got: {events:?}"
        );
        assert!(
            events[0].contains("error"),
            "the terminal event must be the error, got: {}",
            events[0]
        );
        assert!(
            !events.iter().any(|event| event.contains("message_stop")),
            "no message_stop may follow an error event, got: {events:?}"
        );
    }
}
