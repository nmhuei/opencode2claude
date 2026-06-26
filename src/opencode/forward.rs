//! Forwarding logic for communicating with the upstream OpenAI-compatible API.
//!
//! Handles synchronous and streaming requests, search tool interception,
//! WARP IP rotation for rate-limit retry, and SSE event construction.

use crate::error::BridgeError;
use crate::handlers::{ContentVal, MessagesRequest};
use crate::opencode::mapper::{extract_search_query, is_web_search_tool, map_anthropic_to_openai};
use crate::opencode::search::SearchClient;
use crate::opencode::types::*;
use crate::sse::SseEventBuilder;
use axum::response::sse::Event;
use futures_util::{Stream, StreamExt};
use reqwest::Client;
use std::collections::HashMap;
use std::convert::Infallible;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info, warn};

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

async fn rotate_warp_ip() {
    info!("Rotating WARP IP address...");
    let _ = tokio::process::Command::new("warp-cli")
        .arg("disconnect")
        .output()
        .await;
    tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;
    let _ = tokio::process::Command::new("warp-cli")
        .arg("connect")
        .output()
        .await;
    tokio::time::sleep(tokio::time::Duration::from_millis(2500)).await;
    info!("WARP IP address rotated successfully.");
}

async fn execute_with_warp_retry(
    client: &Client,
    req_body: &OpenAiRequest,
) -> Result<reqwest::Response, reqwest::Error> {
    let mut retry_count = 0;
    loop {
        let res = client
            .post("https://opencode.ai/zen/v1/chat/completions")
            .json(req_body)
            .send()
            .await;

        match res {
            Ok(response) => {
                let status = response.status();
                // TOO_MANY_REQUESTS is 429, BAD_REQUEST (400) is returned by upstream on free rate limit errors too
                let is_rate_limit = status == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || status == reqwest::StatusCode::BAD_REQUEST;

                if is_rate_limit && retry_count < 3 {
                    retry_count += 1;
                    warn!(
                        "Upstream rate limit or request error hit (status {}). Attempting to rotate WARP IP (Attempt {}/3)...",
                        status, retry_count
                    );
                    rotate_warp_ip().await;
                    continue;
                }
                return Ok(response);
            }
            Err(e) => {
                if retry_count < 3 {
                    retry_count += 1;
                    warn!(
                        "Network error connecting upstream: {}. Attempting to rotate WARP IP (Attempt {}/3)...",
                        e, retry_count
                    );
                    rotate_warp_ip().await;
                    continue;
                }
                return Err(e);
            }
        }
    }
}

// ── API Forwarding Implementations ──

pub async fn forward_to_llm_sync(
    client: &Client,
    mut payload: MessagesRequest,
    model: String,
    search_client: SearchClient,
) -> Result<serde_json::Value, BridgeError> {
    let mut loop_count = 0;
    loop {
        loop_count += 1;
        if loop_count > 5 {
            return Err(BridgeError::UpstreamError(
                "Search loop protection triggered".to_string(),
            ));
        }

        let openai_req = map_anthropic_to_openai(&payload, model.clone());

        info!("Forwarding sync request for model {}", model);

        let res = execute_with_warp_retry(client, &openai_req)
            .await
            .map_err(|e| BridgeError::UpstreamError(e.to_string()))?;

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            error!("Upstream API returned status {}: {}", status, body);
            return Err(BridgeError::UpstreamError(format!(
                "Upstream returned status {}: {}",
                status, body
            )));
        }

        let openai_resp: OpenAiResponse = res
            .json()
            .await
            .map_err(|e| BridgeError::UpstreamError(format!("Failed to parse response: {}", e)))?;

        let choice = openai_resp.choices.first().ok_or_else(|| {
            BridgeError::UpstreamError("No choices returned from upstream".to_string())
        })?;

        // Check if there is an intercepted search tool call
        let mut has_search = false;
        let mut search_tc_id = String::new();
        let mut search_tc_name = String::new();
        let mut search_tc_input = serde_json::Value::Null;
        let mut search_query = String::new();

        if let Some(tool_calls) = &choice.message.tool_calls {
            for tc in tool_calls {
                if is_web_search_tool(&tc.function.name) {
                    has_search = true;
                    search_tc_id = tc.id.clone();
                    search_tc_name = tc.function.name.clone();
                    let input_val: serde_json::Value =
                        serde_json::from_str(&tc.function.arguments)
                            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    search_tc_input = input_val;
                    search_query = extract_search_query(&tc.function.arguments);
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

            // Append assistant's tool call turn
            let mut assistant_content = Vec::new();
            if let Some(reasoning) = &choice.message.reasoning_content {
                if !reasoning.is_empty() {
                    assistant_content.push(
                        serde_json::from_value(serde_json::json!({
                            "type": "text",
                            "text": format!("<thinking>{}</thinking>", reasoning)
                        }))
                        .unwrap(),
                    );
                }
            }
            if let Some(content) = &choice.message.content {
                if !content.is_empty() {
                    assistant_content.push(
                        serde_json::from_value(serde_json::json!({
                            "type": "text",
                            "text": content
                        }))
                        .unwrap(),
                    );
                }
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
        if let Some(text) = &choice.message.content {
            if !text.is_empty() {
                content_blocks.push(serde_json::json!({
                    "type": "text",
                    "text": text
                }));
            }
        }

        // 3. Tool calls
        if let Some(tool_calls) = &choice.message.tool_calls {
            for tc in tool_calls {
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

        let stop_reason = match choice.finish_reason.as_deref() {
            Some("stop") => "end_turn",
            Some("tool_calls") => "tool_use",
            Some("length") => "max_tokens",
            _ => "end_turn",
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
    client: &Client,
    payload: MessagesRequest,
    model: String,
    channel_capacity: usize,
    search_client: SearchClient,
) -> Result<impl Stream<Item = Result<Event, Infallible>>, BridgeError> {
    let (tx, rx) = tokio::sync::mpsc::channel(channel_capacity);
    let msg_id = format!(
        "msg_opencode_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    let builder = SseEventBuilder::new(msg_id, model.clone());
    let client_clone = client.clone();
    let model_clone = model.clone();

    tokio::spawn(async move {
        let mut current_payload = payload;
        let mut loop_count = 0;

        loop {
            loop_count += 1;
            if loop_count > 5 {
                error!("Search loop protection triggered!");
                break;
            }

            let openai_req = map_anthropic_to_openai(&current_payload, model_clone.clone());

            info!(
                "Forwarding stream request for model {} (loop {})",
                model_clone, loop_count
            );

            let res = match execute_with_warp_retry(&client_clone, &openai_req).await {
                Ok(r) => r,
                Err(e) => {
                    error!("Error forwarding upstream request: {}", e);
                    break;
                }
            };

            if !res.status().is_success() {
                let status = res.status();
                let body = res.text().await.unwrap_or_default();
                error!("Upstream API returned status {}: {}", status, body);
                break;
            }

            let mut bytes_stream = res.bytes_stream();
            let mut line_buffer = Vec::new();

            if loop_count == 1 {
                let _ = tx.send(builder.message_start()).await;
            }

            let mut thinking_block_index: Option<usize> = None;
            let mut text_block_index: Option<usize> = None;
            let mut tool_block_indices: HashMap<usize, (usize, String, String)> = HashMap::new();
            let mut next_content_block_index = 0;
            let mut final_stop_reason = "end_turn".to_string();

            let mut intercepting_search = false;
            let mut search_tc_id = String::new();
            let mut search_tc_name = String::new();
            let mut search_tc_args = String::new();
            let mut accumulated_thinking = String::new();
            let mut accumulated_text = String::new();

            let mut stream_failed = false;

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
                                            let idx = match thinking_block_index {
                                                Some(i) => i,
                                                None => {
                                                    let i = next_content_block_index;
                                                    next_content_block_index += 1;
                                                    thinking_block_index = Some(i);
                                                    let start_ev = Event::default()
                                                        .event("content_block_start")
                                                        .json_data(serde_json::json!({
                                                            "type": "content_block_start",
                                                            "index": i,
                                                            "content_block": {"type": "thinking", "thinking": ""}
                                                        }))
                                                        .unwrap_or_else(|_| Event::default().data("{}"));
                                                    let _ = tx.send(start_ev).await;
                                                    i
                                                }
                                            };

                                            let delta_ev = Event::default()
                                                .event("content_block_delta")
                                                .json_data(serde_json::json!({
                                                    "type": "content_block_delta",
                                                    "index": idx,
                                                    "delta": {"type": "thinking_delta", "thinking": reasoning}
                                                }))
                                                .unwrap_or_else(|_| Event::default().data("{}"));
                                            let _ = tx.send(delta_ev).await;
                                        }
                                    }
                                }

                                // 2. Process content (text delta)
                                if let Some(content) = &choice.delta.content {
                                    if !content.is_empty() {
                                        accumulated_text.push_str(content);
                                        if !intercepting_search {
                                            // Close thinking block if open
                                            if let Some(idx) = thinking_block_index {
                                                let stop_ev = Event::default()
                                                    .event("content_block_stop")
                                                    .json_data(serde_json::json!({
                                                        "type": "content_block_stop",
                                                        "index": idx
                                                    }))
                                                    .unwrap_or_else(|_| {
                                                        Event::default().data("{}")
                                                    });
                                                let _ = tx.send(stop_ev).await;
                                                thinking_block_index = None;
                                            }

                                            let idx = match text_block_index {
                                                Some(i) => i,
                                                None => {
                                                    let i = next_content_block_index;
                                                    next_content_block_index += 1;
                                                    text_block_index = Some(i);
                                                    let start_ev = Event::default()
                                                        .event("content_block_start")
                                                        .json_data(serde_json::json!({
                                                            "type": "content_block_start",
                                                            "index": i,
                                                            "content_block": {"type": "text", "text": ""}
                                                        }))
                                                        .unwrap_or_else(|_| Event::default().data("{}"));
                                                    let _ = tx.send(start_ev).await;
                                                    i
                                                }
                                            };

                                            let delta_ev = Event::default()
                                                .event("content_block_delta")
                                                .json_data(serde_json::json!({
                                                    "type": "content_block_delta",
                                                    "index": idx,
                                                    "delta": {"type": "text_delta", "text": content}
                                                }))
                                                .unwrap_or_else(|_| Event::default().data("{}"));
                                            let _ = tx.send(delta_ev).await;
                                        }
                                    }
                                }

                                // 3. Process tool calls
                                if let Some(tool_calls) = &choice.delta.tool_calls {
                                    for tc in tool_calls {
                                        let call_idx = tc.index;

                                        // If not created yet and we have tool id & function name
                                        #[allow(clippy::map_entry)]
                                        if !tool_block_indices.contains_key(&call_idx) {
                                            if let (Some(id), Some(func)) = (&tc.id, &tc.function) {
                                                if let Some(name) = &func.name {
                                                    if is_web_search_tool(name) {
                                                        intercepting_search = true;
                                                        search_tc_id = id.clone();
                                                        search_tc_name = name.clone();
                                                    } else {
                                                        // Close thinking block if open
                                                        if let Some(idx) = thinking_block_index {
                                                            let stop_ev = Event::default()
                                                                .event("content_block_stop")
                                                                .json_data(serde_json::json!({
                                                                    "type": "content_block_stop",
                                                                    "index": idx
                                                                }))
                                                                .unwrap_or_else(|_| {
                                                                    Event::default().data("{}")
                                                                });
                                                            let _ = tx.send(stop_ev).await;
                                                            thinking_block_index = None;
                                                        }
                                                        // Close text block if open
                                                        if let Some(idx) = text_block_index {
                                                            let stop_ev = Event::default()
                                                                .event("content_block_stop")
                                                                .json_data(serde_json::json!({
                                                                    "type": "content_block_stop",
                                                                    "index": idx
                                                                }))
                                                                .unwrap_or_else(|_| {
                                                                    Event::default().data("{}")
                                                                });
                                                            let _ = tx.send(stop_ev).await;
                                                            text_block_index = None;
                                                        }

                                                        let idx = next_content_block_index;
                                                        next_content_block_index += 1;
                                                        tool_block_indices.insert(
                                                            call_idx,
                                                            (idx, id.clone(), name.clone()),
                                                        );

                                                        let start_ev = Event::default()
                                                            .event("content_block_start")
                                                            .json_data(serde_json::json!({
                                                                "type": "content_block_start",
                                                                "index": idx,
                                                                "content_block": {
                                                                    "type": "tool_use",
                                                                    "id": id,
                                                                    "name": name,
                                                                    "input": {}
                                                                }
                                                            }))
                                                            .unwrap_or_else(|_| {
                                                                Event::default().data("{}")
                                                            });
                                                        let _ = tx.send(start_ev).await;
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
                                                tool_block_indices.get(&call_idx)
                                            {
                                                if let Some(func) = &tc.function {
                                                    if let Some(args) = &func.arguments {
                                                        if !args.is_empty() {
                                                            let delta_ev = Event::default()
                                                                .event("content_block_delta")
                                                                .json_data(serde_json::json!({
                                                                    "type": "content_block_delta",
                                                                    "index": *idx,
                                                                    "delta": {
                                                                        "type": "input_json_delta",
                                                                        "partial_json": args
                                                                    }
                                                                }))
                                                                .unwrap_or_else(|_| {
                                                                    Event::default().data("{}")
                                                                });
                                                            let _ = tx.send(delta_ev).await;
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

                // Append assistant turn
                let mut assistant_content = Vec::new();
                if !accumulated_thinking.is_empty() {
                    assistant_content.push(
                        serde_json::from_value(serde_json::json!({
                            "type": "text",
                            "text": format!("<thinking>{}</thinking>", accumulated_thinking)
                        }))
                        .unwrap(),
                    );
                }
                if !accumulated_text.is_empty() {
                    assistant_content.push(
                        serde_json::from_value(serde_json::json!({
                            "type": "text",
                            "text": accumulated_text
                        }))
                        .unwrap(),
                    );
                }
                assistant_content.push(
                    serde_json::from_value(serde_json::json!({
                        "type": "tool_use",
                        "id": search_tc_id,
                        "name": search_tc_name,
                        "input": input_val
                    }))
                    .unwrap(),
                );

                current_payload.messages.push(crate::handlers::Message {
                    role: "assistant".to_string(),
                    content: ContentVal::Multiple(assistant_content),
                });

                // Append tool result turn
                let tool_result_content = vec![serde_json::from_value(serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": search_tc_id,
                    "name": search_tc_name,
                    "content": search_results
                }))
                .unwrap()];
                current_payload.messages.push(crate::handlers::Message {
                    role: "user".to_string(),
                    content: ContentVal::Multiple(tool_result_content),
                });

                // Loop again with updated history to fetch search-informed response
                continue;
            }

            // Close any remaining active content blocks
            if let Some(idx) = thinking_block_index {
                let stop_ev = Event::default()
                    .event("content_block_stop")
                    .json_data(serde_json::json!({
                        "type": "content_block_stop",
                        "index": idx
                    }))
                    .unwrap_or_else(|_| Event::default().data("{}"));
                let _ = tx.send(stop_ev).await;
            }
            if let Some(idx) = text_block_index {
                let stop_ev = Event::default()
                    .event("content_block_stop")
                    .json_data(serde_json::json!({
                        "type": "content_block_stop",
                        "index": idx
                    }))
                    .unwrap_or_else(|_| Event::default().data("{}"));
                let _ = tx.send(stop_ev).await;
            }
            for (_, (idx, _, _)) in tool_block_indices {
                let stop_ev = Event::default()
                    .event("content_block_stop")
                    .json_data(serde_json::json!({
                        "type": "content_block_stop",
                        "index": idx
                    }))
                    .unwrap_or_else(|_| Event::default().data("{}"));
                let _ = tx.send(stop_ev).await;
            }

            // Send final message_delta and message_stop
            let delta_ev = Event::default()
                .event("message_delta")
                .json_data(serde_json::json!({
                    "type": "message_delta",
                    "delta": {
                        "stop_reason": final_stop_reason,
                        "stop_sequence": null
                    },
                    "usage": {"output_tokens": 0}
                }))
                .unwrap_or_else(|_| Event::default().data("{}"));
            let _ = tx.send(delta_ev).await;

            let stop_ev = Event::default()
                .event("message_stop")
                .json_data(serde_json::json!({
                    "type": "message_stop"
                }))
                .unwrap_or_else(|_| Event::default().data("{}"));
            let _ = tx.send(stop_ev).await;
            break;
        }
    });

    Ok(ReceiverStream::new(rx).map(Ok))
}
