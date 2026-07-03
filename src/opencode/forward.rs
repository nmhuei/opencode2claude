//! Forwarding logic for communicating with the upstream OpenAI-compatible API.
//!
//! Handles synchronous and streaming requests, search tool interception,
//! WARP IP rotation for rate-limit retry, and SSE event construction.

use crate::error::BridgeError;
use crate::handlers::{ContentVal, MessagesRequest};
use crate::opencode::mapper::{extract_search_query, is_web_search_tool, map_anthropic_to_openai};
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
use tracing::{error, info};

/// Maximum size of the DSML streaming pre-buffer (256KB).
/// Prevents unbounded memory growth from long text prefix before the
/// closing <｜DSML｜tool_calls> tag is found.
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
        if loop_count >= max_search_loops {
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
            let (cleaned, calls) = extract_and_clean_dsml(text);
            cleaned_message_content = Some(cleaned);
            dsml_tool_calls = calls;
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

            // Extract thinking and text from current response
            let thinking = choice.message.reasoning_content.as_deref().unwrap_or("");
            let text = cleaned_message_content
                .as_deref()
                .map(strip_system_tags)
                .unwrap_or_default();

            inject_search_results(
                &mut payload,
                &search_results,
                thinking,
                &text,
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
            let cleaned = strip_system_tags(text);
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

            loop {
                // Check if client disconnected
                if cancel_token_spawn.is_cancelled() {
                    info!("Client disconnected — cancelling streaming task");
                    break;
                }

                loop_count += 1;
                if loop_count >= max_search_loops {
                    error!("Search loop protection triggered!");
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
                        let _ = tx.send(error_ev).await;
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
                    let _ = tx.send(error_ev).await;
                    break;
                }

                let mut bytes_stream = res.bytes_stream();
                let mut line_buffer = Vec::new();

                if loop_count == 1 {
                    let input_tokens = estimate_input_tokens(&current_payload);
                    let _ = tx.send(builder.message_start(input_tokens)).await;
                }

                let mut tracker = SseBlockTracker::new();
                let mut final_stop_reason = "end_turn".to_string();

                let mut intercepting_search = false;
                let mut search_tc_id = String::new();
                let mut search_tc_name = String::new();
                let mut search_tc_args = String::new();
                let mut accumulated_thinking = String::new();
                let mut accumulated_text = String::new();

                let mut stream_failed = false;
                let mut has_emitted_tool_use = false;
                let mut dsml_mode = false;
                let mut dsml_stream_buffer = String::new();
                let mut text_stream_buffer = String::new();

                while let Some(chunk_res) = bytes_stream.next().await {
                    let chunk = match chunk_res {
                        Ok(c) => c,
                        Err(e) => {
                            error!("Error reading chunk from upstream: {}", e);
                            stream_failed = true;
                            break;
                        }
                    };
                    line_buffer.extend_from_slice(&chunk);

                    while let Some(pos) = line_buffer.iter().position(|&b| b == b'\n') {
                        let line_bytes = line_buffer.drain(..pos + 1).collect::<Vec<u8>>();
                        let line = String::from_utf8_lossy(&line_bytes);
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }

                        if let Some(stripped) = line.strip_prefix("data:") {
                            let data_str = stripped.trim();
                            if data_str == "[DONE]" {
                                break;
                            }

                            if let Ok(chunk) = serde_json::from_str::<OpenAiStreamChunk>(data_str) {
                                if let Some(choice) = chunk.choices.first() {
                                    if let Some(reason) = &choice.finish_reason {
                                        final_stop_reason = match reason.as_str() {
                                            "stop" => "end_turn".to_string(),
                                            "tool_calls" => "tool_use".to_string(),
                                            "length" => "max_tokens".to_string(),
                                            _ => "end_turn".to_string(),
                                        };
                                    }

                                    // 1. Process reasoning_content (thinking delta)
                                    if let Some(reasoning) = &choice.delta.reasoning_content {
                                        if !reasoning.is_empty() {
                                            accumulated_thinking.push_str(reasoning);
                                            if !intercepting_search {
                                                let (thinking_idx, thinking_is_new, closed_text) =
                                                    tracker.ensure_thinking();
                                                if let Some(closed) = closed_text {
                                                    let _ = tx
                                                        .send(crate::sse::emit_block_stop(closed))
                                                        .await;
                                                }
                                                if thinking_is_new {
                                                    let _ = tx
                                                        .send(builder.content_block_start(
                                                            thinking_idx,
                                                            "thinking",
                                                            None,
                                                            None,
                                                        ))
                                                        .await;
                                                }

                                                let _ = tx
                                                    .send(
                                                        builder.thinking_delta(
                                                            thinking_idx,
                                                            reasoning,
                                                        ),
                                                    )
                                                    .await;
                                            }
                                        }
                                    }

                                    // 2. Process content (text delta)
                                    if let Some(content) = &choice.delta.content {
                                        if dsml_mode {
                                            dsml_stream_buffer.push_str(content);
                                            // Enforce DSML buffer cap — prevents OOM from long text prefix
                                            if dsml_stream_buffer.len() > MAX_DSML_BUFFER_SIZE {
                                                error!(
                                                "DSML stream buffer exceeded {} bytes — truncating",
                                                MAX_DSML_BUFFER_SIZE
                                            );
                                                dsml_stream_buffer = String::new();
                                                dsml_mode = false;
                                            }
                                            if let Some(end_pos) =
                                                dsml_stream_buffer.find("</｜DSML｜tool_calls>")
                                            {
                                                let end_idx =
                                                    end_pos + "</｜DSML｜tool_calls>".len();
                                                let dsml_block = &dsml_stream_buffer[..end_idx];
                                                let remaining =
                                                    dsml_stream_buffer[end_idx..].to_string();

                                                let calls = parse_dsml_tool_calls(dsml_block);
                                                for call in calls {
                                                    has_emitted_tool_use = true;
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
                                                        let _ = tx
                                                            .send(crate::sse::emit_block_stop(idx))
                                                            .await;
                                                    }
                                                    if let Some(idx) = tracker.close_text() {
                                                        let _ = tx
                                                            .send(crate::sse::emit_block_stop(idx))
                                                            .await;
                                                    }

                                                    let _ = tx
                                                        .send(builder.content_block_start(
                                                            call_idx,
                                                            "tool_use",
                                                            Some(&tool_id),
                                                            Some(&get_correct_tool_name(
                                                                &call.name,
                                                                &current_payload,
                                                            )),
                                                        ))
                                                        .await;

                                                    let args_str =
                                                        serde_json::to_string(&call.arguments)
                                                            .unwrap_or_default();
                                                    let _ = tx
                                                        .send(
                                                            builder.input_json_delta(
                                                                call_idx, &args_str,
                                                            ),
                                                        )
                                                        .await;

                                                    let _ = tx
                                                        .send(crate::sse::emit_block_stop(call_idx))
                                                        .await;
                                                }

                                                dsml_stream_buffer = String::new();
                                                dsml_mode = false;

                                                if !remaining.is_empty() {
                                                    text_stream_buffer.push_str(&remaining);
                                                }
                                            }
                                        } else {
                                            text_stream_buffer.push_str(content);
                                        }

                                        if !dsml_mode {
                                            if let Some(start_pos) =
                                                text_stream_buffer.find("<｜DSML｜tool_calls>")
                                            {
                                                let text_to_yield =
                                                    &text_stream_buffer[..start_pos];
                                                let remainder = &text_stream_buffer[start_pos..];

                                                let cleaned = strip_system_tags(text_to_yield);
                                                if !cleaned.is_empty() {
                                                    accumulated_text.push_str(&cleaned);
                                                    if !intercepting_search {
                                                        if let Some(idx) = tracker.close_thinking()
                                                        {
                                                            let _ = tx
                                                                .send(crate::sse::emit_block_stop(
                                                                    idx,
                                                                ))
                                                                .await;
                                                        }

                                                        let (text_idx, text_is_new, _closed) =
                                                            tracker.ensure_text();
                                                        if text_is_new {
                                                            let _ = tx
                                                                .send(builder.content_block_start(
                                                                    text_idx, "text", None, None,
                                                                ))
                                                                .await;
                                                        }
                                                        let _ = tx
                                                            .send(
                                                                builder
                                                                    .text_delta(text_idx, &cleaned),
                                                            )
                                                            .await;
                                                    }
                                                }

                                                dsml_mode = true;
                                                dsml_stream_buffer = remainder.to_string();
                                                text_stream_buffer = String::new();
                                            } else {
                                                let (to_yield, pending) =
                                                    split_pending_text(&text_stream_buffer);
                                                let cleaned = strip_system_tags(&to_yield);
                                                if !cleaned.is_empty() {
                                                    accumulated_text.push_str(&cleaned);
                                                    if !intercepting_search {
                                                        if let Some(idx) = tracker.close_thinking()
                                                        {
                                                            let _ = tx
                                                                .send(crate::sse::emit_block_stop(
                                                                    idx,
                                                                ))
                                                                .await;
                                                        }

                                                        let (text_idx, text_is_new, _closed) =
                                                            tracker.ensure_text();
                                                        if text_is_new {
                                                            let _ = tx
                                                                .send(builder.content_block_start(
                                                                    text_idx, "text", None, None,
                                                                ))
                                                                .await;
                                                        }
                                                        let _ = tx
                                                            .send(
                                                                builder
                                                                    .text_delta(text_idx, &cleaned),
                                                            )
                                                            .await;
                                                    }
                                                }
                                                text_stream_buffer = pending;
                                            }
                                        }
                                    }

                                    // 3. Process tool calls
                                    if let Some(tool_calls) = &choice.delta.tool_calls {
                                        for tc in tool_calls {
                                            let call_idx = tc.index;

                                            // If not created yet and we have tool id & function name
                                            #[allow(clippy::map_entry)]
                                            if tracker.tool_idx(call_idx).is_none() {
                                                if let (Some(id), Some(func)) =
                                                    (&tc.id, &tc.function)
                                                {
                                                    if let Some(name) = &func.name {
                                                        if is_web_search_tool(name) {
                                                            intercepting_search = true;
                                                            search_tc_id = id.clone();
                                                            search_tc_name = name.clone();
                                                        } else {
                                                            // Close thinking block if open
                                                            if let Some(idx) =
                                                                tracker.close_thinking()
                                                            {
                                                                let _ = tx
                                                                    .send(
                                                                        crate::sse::emit_block_stop(
                                                                            idx,
                                                                        ),
                                                                    )
                                                                    .await;
                                                            }
                                                            if let Some(idx) = tracker.close_text()
                                                            {
                                                                let _ = tx
                                                                    .send(
                                                                        crate::sse::emit_block_stop(
                                                                            idx,
                                                                        ),
                                                                    )
                                                                    .await;
                                                            }

                                                            let (_block_idx, _closed_t, _closed_x) =
                                                                tracker.open_tool_use(
                                                                    call_idx,
                                                                    id.clone(),
                                                                    get_correct_tool_name(
                                                                        name,
                                                                        &current_payload,
                                                                    ),
                                                                );

                                                            let _ = tx
                                                                .send(builder.content_block_start(
                                                                    _block_idx,
                                                                    "tool_use",
                                                                    Some(id),
                                                                    Some(&get_correct_tool_name(
                                                                        name,
                                                                        &current_payload,
                                                                    )),
                                                                ))
                                                                .await;
                                                            has_emitted_tool_use = true;
                                                        }
                                                    }
                                                }
                                            }

                                            // Send arguments delta if present
                                            if intercepting_search {
                                                if let Some(func) = &tc.function {
                                                    if let Some(args) = &func.arguments {
                                                        search_tc_args.push_str(args);
                                                    }
                                                }
                                            } else {
                                                if let Some((idx, _, _)) =
                                                    tracker.tool_idx(call_idx)
                                                {
                                                    if let Some(func) = &tc.function {
                                                        if let Some(args) = &func.arguments {
                                                            if !args.is_empty() {
                                                                let _ = tx
                                                                    .send(builder.input_json_delta(
                                                                        *idx, args,
                                                                    ))
                                                                    .await;
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if stream_failed {
                    break;
                }

                if intercepting_search {
                    // Extract query from accumulated arguments
                    let input_val: serde_json::Value = serde_json::from_str(&search_tc_args)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    let search_query = extract_search_query(&search_tc_args);

                    info!(
                        "Intercepted stream search tool call. Query: '{}'",
                        search_query
                    );
                    let search_results = search_client.search(&search_query).await;
                    info!("Search completed. Results length: {}", search_results.len());

                    inject_search_results(
                        &mut current_payload,
                        &search_results,
                        &accumulated_thinking,
                        &accumulated_text,
                        &search_tc_id,
                        &search_tc_name,
                        &input_val,
                    );

                    // Loop again with updated history to fetch search-informed response
                    continue;
                }

                // Flush any remaining text in text_stream_buffer
                let cleaned = strip_system_tags(&text_stream_buffer);
                if !cleaned.is_empty() {
                    accumulated_text.push_str(&cleaned);
                    if !intercepting_search {
                        if let Some(idx) = tracker.close_thinking() {
                            let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
                        }

                        let (text_idx, text_is_new, _closed) = tracker.ensure_text();
                        if text_is_new {
                            let _ = tx
                                .send(builder.content_block_start(text_idx, "text", None, None))
                                .await;
                        }
                        let _ = tx.send(builder.text_delta(text_idx, &cleaned)).await;
                    }
                }

                // Flush/parse any remaining unclosed DSML block in dsml_stream_buffer
                if dsml_mode && !dsml_stream_buffer.is_empty() {
                    let calls = parse_dsml_tool_calls(&dsml_stream_buffer);
                    for call in calls {
                        has_emitted_tool_use = true;
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
                            .send(builder.content_block_start(
                                call_idx,
                                "tool_use",
                                Some(&tool_id),
                                Some(&get_correct_tool_name(&call.name, &current_payload)),
                            ))
                            .await;

                        let args_str = serde_json::to_string(&call.arguments).unwrap_or_default();
                        let _ = tx.send(builder.input_json_delta(call_idx, &args_str)).await;

                        let _ = tx.send(crate::sse::emit_block_stop(call_idx)).await;
                    }
                }

                // Close any remaining active content blocks
                for (_, idx) in tracker.close_all() {
                    let _ = tx.send(crate::sse::emit_block_stop(idx)).await;
                }

                let stop_reason = if has_emitted_tool_use {
                    "tool_use".to_string()
                } else {
                    final_stop_reason
                };

                // Send final message_delta and message_stop
                let output_tokens = estimate_string_tokens(&accumulated_thinking)
                    + estimate_string_tokens(&accumulated_text);
                let output_tokens = if output_tokens == 0 && has_emitted_tool_use {
                    15
                } else {
                    output_tokens
                };

                let _ = tx
                    .send(builder.message_delta(&stop_reason, output_tokens))
                    .await;

                let _ = tx.send(builder.message_stop()).await;
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

/// Estimate the number of tokens for a given text using a heuristic formula.
///
/// This is a rough estimation, NOT a real tokenizer:
/// - Whitespace: 0.25 tokens per char
/// - New alphanumeric word: 1.0 tokens
/// - Subsequent alphanumeric chars: 0.22 tokens
/// - Non-alphanumeric chars: 0.5 tokens
///
/// The formula is derived from empirical observation of typical English text.
/// For production token accounting, prefer a proper tokenizer crate.
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

    #[test]
    fn test_estimate_string_tokens_empty() {
        assert_eq!(estimate_string_tokens(""), 0);
    }

    #[test]
    fn test_estimate_string_tokens_whitespace() {
        // 4 spaces @ 0.25 each = 1.0 → rounds to 1
        assert_eq!(estimate_string_tokens("    "), 1);
    }

    #[test]
    fn test_estimate_string_tokens_basic_word() {
        // "hello" → 1 start + 4*0.22 = 1.88 → rounds to 2
        let result = estimate_string_tokens("hello");
        assert!(
            result >= 1,
            "expected at least 1 token for 'hello', got {}",
            result
        );
    }

    #[test]
    fn test_estimate_string_tokens_sentence() {
        let result = estimate_string_tokens("Hello world");
        // "Hello" → 1 + 4*0.22 = 1.88, space → 0.25, "world" → 1 + 4*0.22 = 1.88
        // total ≈ 4.01 → rounds to 4
        assert_eq!(result, 4);
    }

    #[test]
    fn test_estimate_string_tokens_cjk() {
        // CJK chars are non-ASCII, non-alphanumeric → 0.5 each
        let result = estimate_string_tokens("你好世界");
        // 4 chars * 0.5 = 2.0 → rounds to 2
        assert_eq!(result, 2);
    }

    #[test]
    fn test_estimate_string_tokens_punctuation() {
        // Each punctuation char = 0.5 tokens. 3 × 0.5 = 1.5 → round() = 2
        assert_eq!(estimate_string_tokens("..."), 2);
    }

    #[test]
    fn test_estimate_input_tokens_empty_payload() {
        let payload = MessagesRequest {
            model: None,
            messages: vec![],
            system: None,
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: None,
            max_tokens: None,
        };
        // Empty payload → total_tokens == 0 → returns 100 (fallback)
        assert_eq!(estimate_input_tokens(&payload), 100);
    }

    #[test]
    fn test_estimate_input_tokens_with_system() {
        let payload = MessagesRequest {
            model: None,
            messages: vec![],
            system: Some(serde_json::json!("You are a helpful assistant")),
            tools: None,
            tool_choice: None,
            stream: false,
            temperature: None,
            max_tokens: None,
        };
        let result = estimate_input_tokens(&payload);
        assert!(
            result > 0,
            "expected > 0 tokens for system prompt, got {}",
            result
        );
    }
}
