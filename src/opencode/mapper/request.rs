//! Construction of the OpenAI-compatible upstream request.

use super::helpers::{extract_system_prompt, map_model_name, tool_result_content_to_string};
use super::policy::{
    include_reasoning_for_stream, is_compact_request, normalize_upstream_max_tokens,
};
use crate::handlers::{ContentVal, MessagesRequest};
use crate::opencode::types::*;
use std::collections::HashMap;

/// Map Anthropic request values into a standard OpenAI payload using the
/// default reasoning-stream token floor. Tests and compatibility callers use
/// this wrapper; runtime code passes the resolved policy explicitly.
pub fn map_anthropic_to_openai(payload: &MessagesRequest, model: String) -> OpenAiRequest {
    map_anthropic_to_openai_with_policy(
        payload,
        model,
        super::policy::DEFAULT_MIN_REASONING_STREAM_TOKENS,
    )
}

pub fn map_anthropic_to_openai_with_policy(
    payload: &MessagesRequest,
    model: String,
    minimum_reasoning_stream_tokens: u32,
) -> OpenAiRequest {
    let mapped_model = map_model_name(&model);
    let mut openai_messages = Vec::new();

    // Build a map of tool_use_id -> name from previous assistant messages
    let mut tool_name_map = HashMap::new();
    for msg in &payload.messages {
        if msg.role == "assistant" {
            if let ContentVal::Multiple(blocks) = &msg.content {
                for block in blocks {
                    if block.content_type == "tool_use" {
                        if let (Some(id), Some(name)) = (&block.id, &block.name) {
                            tool_name_map.insert(id.clone(), name.clone());
                        }
                    }
                }
            }
        }
    }

    // 1. System Prompt
    if let Some(system_val) = &payload.system {
        let system = extract_system_prompt(system_val);
        if !system.is_empty() {
            openai_messages.push(OpenAiMessage {
                role: "system".to_string(),
                content: Some(system),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
    }

    // 2. Messages conversation turns
    for msg in &payload.messages {
        match &msg.content {
            ContentVal::Single(text) => {
                openai_messages.push(OpenAiMessage {
                    role: msg.role.clone(),
                    content: Some(text.clone()),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                });
            }
            ContentVal::Multiple(blocks) => {
                if msg.role == "user" {
                    let mut user_text = String::new();
                    for block in blocks {
                        match block.content_type.as_str() {
                            "text" => {
                                if let Some(t) = &block.text {
                                    if !user_text.is_empty() {
                                        user_text.push('\n');
                                    }
                                    user_text.push_str(t);
                                }
                            }
                            "tool_result" => {
                                let name = block.name.clone().or_else(|| {
                                    block
                                        .tool_use_id
                                        .as_ref()
                                        .and_then(|id| tool_name_map.get(id).cloned())
                                });
                                let content_str = block
                                    .content
                                    .as_ref()
                                    .map(tool_result_content_to_string)
                                    .unwrap_or_default();

                                let mapped_model =
                                    map_model_name(&payload.model.clone().unwrap_or_default());
                                if mapped_model.contains("-free") {
                                    // Fallback: convert tool result into a standard user message prompt block
                                    if !user_text.is_empty() {
                                        user_text.push('\n');
                                    }
                                    user_text.push_str(&format!(
                                        "[Tool Result for tool '{}']\n{}",
                                        name.unwrap_or_else(|| "unknown".to_string()),
                                        content_str
                                    ));
                                } else {
                                    openai_messages.push(OpenAiMessage {
                                        role: "tool".to_string(),
                                        content: Some(content_str),
                                        reasoning_content: None,
                                        tool_calls: None,
                                        tool_call_id: block.tool_use_id.clone(),
                                        name,
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                    if !user_text.is_empty() {
                        openai_messages.push(OpenAiMessage {
                            role: "user".to_string(),
                            content: Some(user_text),
                            reasoning_content: None,
                            tool_calls: None,
                            tool_call_id: None,
                            name: None,
                        });
                    }
                } else if msg.role == "assistant" {
                    let mut assistant_text = String::new();
                    let mut reasoning_content = String::new();
                    let mut tool_calls = Vec::new();
                    for block in blocks {
                        match block.content_type.as_str() {
                            "thinking" => {
                                if let Some(t) = &block.text {
                                    reasoning_content.push_str(t);
                                }
                            }
                            "text" => {
                                if let Some(t) = &block.text {
                                    if !assistant_text.is_empty() {
                                        assistant_text.push('\n');
                                    }
                                    assistant_text.push_str(t);
                                }
                            }
                            "tool_use" => {
                                if let (Some(id), Some(name), Some(input)) =
                                    (&block.id, &block.name, &block.input)
                                {
                                    if mapped_model.contains("-free") {
                                        if !assistant_text.is_empty() {
                                            assistant_text.push('\n');
                                        }
                                        assistant_text.push_str(&format!(
                                            "[Requesting Tool execution: '{}' with arguments: {}]",
                                            name,
                                            serde_json::to_string(input).unwrap_or_default()
                                        ));
                                    } else {
                                        tool_calls.push(OpenAiToolCall {
                                            id: id.clone(),
                                            tool_type: "function".to_string(),
                                            function: OpenAiFunctionCall {
                                                name: name.clone(),
                                                arguments: serde_json::to_string(input)
                                                    .unwrap_or_default(),
                                            },
                                        });
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    openai_messages.push(OpenAiMessage {
                        role: "assistant".to_string(),
                        content: if assistant_text.is_empty() {
                            None
                        } else {
                            Some(assistant_text)
                        },
                        reasoning_content: if reasoning_content.is_empty() {
                            None
                        } else {
                            Some(reasoning_content)
                        },
                        tool_calls: if tool_calls.is_empty() {
                            None
                        } else {
                            Some(tool_calls)
                        },
                        tool_call_id: None,
                        name: None,
                    });
                }
            }
        }
    }

    // 3. Tools mapping
    let tools = payload.tools.as_ref().map(|t_list| {
        t_list
            .iter()
            .map(|t| OpenAiTool {
                tool_type: "function".to_string(),
                function: OpenAiFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.input_schema.clone(),
                },
            })
            .collect()
    });

    // 4. Tool Choice mapping
    let tool_choice = payload.tool_choice.as_ref().map(|tc| {
        if let Some(tc_str) = tc.as_str() {
            serde_json::Value::String(tc_str.to_string())
        } else if let Some(tc_obj) = tc.as_object() {
            if let Some(t_type) = tc_obj.get("type").and_then(|t| t.as_str()) {
                match t_type {
                    "auto" => serde_json::Value::String("auto".to_string()),
                    "any" => serde_json::Value::String("required".to_string()),
                    "tool" => {
                        if let Some(t_name) = tc_obj.get("name").and_then(|n| n.as_str()) {
                            serde_json::json!({
                                "type": "function",
                                "function": { "name": t_name }
                            })
                        } else {
                            serde_json::Value::String("auto".to_string())
                        }
                    }
                    _ => serde_json::Value::String("auto".to_string()),
                }
            } else {
                serde_json::Value::String("auto".to_string())
            }
        } else {
            serde_json::Value::String("auto".to_string())
        }
    });

    let is_compact = is_compact_request(payload);
    let max_tokens = normalize_upstream_max_tokens(
        payload.max_tokens,
        payload.stream,
        &mapped_model,
        is_compact,
        minimum_reasoning_stream_tokens,
    );
    let include_reasoning = include_reasoning_for_stream(payload.stream, &mapped_model, is_compact);

    OpenAiRequest {
        model: mapped_model,
        messages: openai_messages,
        tools,
        tool_choice,
        stream: payload.stream,
        temperature: payload.temperature,
        max_tokens,
        include_reasoning,
    }
}
