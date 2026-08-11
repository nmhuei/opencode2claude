//! Construction of the OpenAI-compatible upstream request.

use super::helpers::{
    extract_system_prompt, is_bridge_search_tool, map_model_name, tool_result_content_to_string,
};
use super::policy::{
    dropped_schema_system_instruction, include_reasoning_for_stream, is_compact_request,
    is_deepseek_v4_flash_free_model, is_deepseek_v4_model, normalize_reasoning_effort,
    normalize_response_format, normalize_upstream_max_tokens, normalize_upstream_thinking,
};
use crate::handlers::{ContentVal, MessagesRequest};
use crate::opencode::types::*;
use std::collections::HashMap;

const DEEPSEEK_FREE_REASONING_HYGIENE: &str = "Reasoning hygiene: never restart, restate, or repeat the same plan or interpretation in the reasoning channel. State each plan once. After deciding the next action, perform it immediately. Do not repeatedly announce that you are about to use a tool.";

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
    // DFLASH thinking requests require every historical assistant tool-call
    // message to carry non-empty reasoning_content, for both sync and stream.
    let synthesize_missing_tool_reasoning =
        is_deepseek_v4_flash_free_model(&mapped_model) && payload.thinking_enabled() == Some(true);
    let needs_reasoning_hygiene = payload.stream
        && mapped_model.ends_with("-free")
        && is_deepseek_v4_model(&mapped_model)
        && payload.thinking_enabled() == Some(true);
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
    let mut system = payload
        .system
        .as_ref()
        .map(extract_system_prompt)
        .unwrap_or_default();
    for message in &payload.messages {
        if message.role != "system" {
            continue;
        }
        let hook_context = message_content_text(&message.content);
        if hook_context.is_empty() {
            continue;
        }
        if !system.is_empty() {
            system.push_str("\n\n");
        }
        system.push_str(&hook_context);
    }
    if let Some(remaining_agents) = explicit_fanout_agents_remaining(payload) {
        if !system.is_empty() {
            system.push_str("\n\n");
        }
        system.push_str(&format!(
            "Claude Code compatibility requirement: the user explicitly requested a fan-out of subagents. Before giving a final answer, issue exactly {remaining_agents} additional Agent tool call(s) with distinct, non-overlapping research scopes, for a maximum of two Agent calls total. If more than one call remains, emit them in the same assistant turn so Claude Code can execute them concurrently. Set run_in_background=false so every Agent returns its full tool_result to this same Claude Code session. In every Agent prompt, require at most two WebSearch calls with distinct queries, real source URLs, and a direct structured final report; explicitly forbid that subagent from spawning Agent children or requesting Bash, Read, TaskOutput, or other unavailable tools. Do not replace the required Agent calls with parent-level WebSearch or WebFetch. A response with fewer than two total Agent calls is incomplete: after the first Agent tool_result, immediately call the remaining Agent before writing any final answer. After both Agent tool_result messages are available, synthesize the combined findings immediately without launching more agents."
        ));
    }
    if needs_reasoning_hygiene {
        if !system.is_empty() {
            system.push_str("\n\n");
        }
        system.push_str(DEEPSEEK_FREE_REASONING_HYGIENE);
    }
    if let Some(schema_instruction) = dropped_schema_system_instruction(payload, &mapped_model) {
        if !system.is_empty() {
            system.push_str("\n\n");
        }
        system.push_str(&schema_instruction);
    }
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

    // 2. Messages conversation turns
    for msg in &payload.messages {
        if msg.role == "system" {
            continue;
        }
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

                                openai_messages.push(OpenAiMessage {
                                    role: "tool".to_string(),
                                    content: Some(content_str),
                                    reasoning_content: None,
                                    tool_calls: None,
                                    tool_call_id: block.tool_use_id.clone(),
                                    name,
                                });
                            }
                            other => {
                                // Non-text content (image, document, ...) cannot
                                // be converted for the upstream provider. Keep a
                                // compact marker so the model learns the user
                                // attached something instead of the block
                                // silently vanishing from the conversation.
                                if !user_text.is_empty() {
                                    user_text.push('\n');
                                }
                                user_text
                                    .push_str(&format!("[attached {other} block not forwarded]"));
                            }
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
                                if let Some(thinking) =
                                    block.thinking.as_ref().or(block.text.as_ref())
                                {
                                    reasoning_content.push_str(thinking);
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
                        reasoning_content: if !reasoning_content.is_empty() {
                            Some(reasoning_content)
                        } else if synthesize_missing_tool_reasoning && !tool_calls.is_empty() {
                            Some("Tool call continuation.".to_string())
                        } else {
                            None
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
    let upstream_thinking = normalize_upstream_thinking(payload, &mapped_model);
    let effective_thinking = upstream_thinking
        .as_ref()
        .map(|config| config.thinking_type == "enabled")
        .or_else(|| payload.thinking_enabled());
    let max_tokens = normalize_upstream_max_tokens(
        payload.max_tokens,
        payload.stream,
        &mapped_model,
        is_compact,
        minimum_reasoning_stream_tokens,
    );
    let include_reasoning = include_reasoning_for_stream(
        payload.stream,
        &mapped_model,
        is_compact,
        effective_thinking,
    );
    let reasoning_effort =
        normalize_reasoning_effort(payload, &mapped_model, upstream_thinking.as_ref());
    let response_format = normalize_response_format(payload, &mapped_model);
    let deepseek_thinking = is_deepseek_v4_model(&mapped_model) && effective_thinking == Some(true);
    // Only bridge-intercepted search tools need serial single-call emission:
    // the executor splits mixed batches and collapses pure search batches, so
    // other tool sets keep the upstream default (parallel), which the fan-out
    // instruction above relies on for same-turn concurrent Agent calls.
    let parallel_tool_calls = tools
        .as_ref()
        .filter(|tools: &&Vec<OpenAiTool>| {
            tools
                .iter()
                .any(|tool| is_bridge_search_tool(&tool.function.name))
        })
        .map(|_| false);

    OpenAiRequest {
        model: mapped_model,
        messages: openai_messages,
        tools,
        parallel_tool_calls,
        tool_choice: if deepseek_thinking { None } else { tool_choice },
        stream: payload.stream,
        temperature: if deepseek_thinking {
            None
        } else {
            payload.temperature
        },
        top_p: if deepseek_thinking {
            None
        } else {
            payload.top_p
        },
        stop: payload.stop_sequences.clone(),
        max_tokens,
        thinking: upstream_thinking,
        reasoning_effort,
        response_format,
        include_reasoning,
    }
}

fn message_content_text(content: &ContentVal) -> String {
    match content {
        ContentVal::Single(text) => text.clone(),
        ContentVal::Multiple(blocks) => blocks
            .iter()
            .filter(|block| block.content_type == "text")
            .filter_map(|block| block.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn explicit_fanout_agents_remaining(payload: &MessagesRequest) -> Option<usize> {
    let agent_available = payload.tools.as_ref().is_some_and(|tools| {
        tools
            .iter()
            .any(|tool| tool.name.eq_ignore_ascii_case("Agent"))
    });
    if !agent_available {
        return None;
    }

    let mut user_text = String::new();
    for message in &payload.messages {
        if message.role != "user" {
            continue;
        }
        match &message.content {
            ContentVal::Single(text) => {
                user_text.push_str(text);
                user_text.push('\n');
            }
            ContentVal::Multiple(blocks) => {
                for block in blocks {
                    if block.content_type == "text" {
                        if let Some(text) = &block.text {
                            user_text.push_str(text);
                            user_text.push('\n');
                        }
                    }
                }
            }
        }
    }
    let normalized = user_text.to_lowercase();
    let explicit = [
        "fan sub agent",
        "fan subagent",
        "fan-out subagent",
        "fan out subagent",
        "parallel subagent",
        "multiple subagent",
        "nhiều subagent",
        "chia subagent",
        "subagent song song",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    if !explicit {
        return None;
    }

    // A trigger phrase with a negation means the user asked NOT to fan out
    // ("do not fan out subagents"). Injecting the mandate would override the
    // user's actual instruction.
    let negated = [
        "do not fan",
        "don't fan",
        "dont fan",
        "no fan",
        "not fan",
        "without fan",
        "do not use subagent",
        "don't use subagent",
        "dont use subagent",
        "no subagent",
        "not use subagent",
        "không fan",
        "đừng fan",
        "không dùng subagent",
        "đừng dùng subagent",
        "không subagent",
        "đừng subagent",
        "không chia subagent",
        "đừng chia subagent",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    if negated {
        return None;
    }

    let prior_agent_calls = payload
        .messages
        .iter()
        .filter(|message| message.role == "assistant")
        .filter_map(|message| match &message.content {
            ContentVal::Multiple(blocks) => Some(blocks),
            ContentVal::Single(_) => None,
        })
        .flatten()
        .filter(|block| {
            block.content_type == "tool_use"
                && block
                    .name
                    .as_deref()
                    .is_some_and(|name| name.eq_ignore_ascii_case("Agent"))
        })
        .count();

    (prior_agent_calls < 2).then_some(2 - prior_agent_calls)
}
