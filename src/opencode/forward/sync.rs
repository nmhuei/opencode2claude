//! Non-streaming upstream forwarding.

use super::common::{get_correct_tool_name, inject_search_results};
use crate::error::BridgeError;
use crate::handlers::MessagesRequest;
use crate::opencode::mapper::{
    extract_search_query, is_compact_request, is_web_search_tool,
    map_anthropic_to_openai_with_policy,
};
use crate::opencode::retry::execute_with_warp_retry;
use crate::opencode::sanitize::{extract_and_clean_dsml, strip_system_tags};
use crate::opencode::search::SearchClient;
use crate::opencode::types::*;
use crate::state::AppState;
use tracing::{error, info};

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

        let openai_req = map_anthropic_to_openai_with_policy(
            &payload,
            model.clone(),
            state.config.protocol.min_reasoning_stream_tokens,
        );

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
                .map(|t| {
                    if is_compact {
                        t.to_string()
                    } else {
                        strip_system_tags(t)
                    }
                })
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
            let cleaned = if is_compact {
                text.to_string()
            } else {
                strip_system_tags(text)
            };
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
