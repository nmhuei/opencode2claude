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
    let fanout_requirement = explicit_fanout_requirement(payload);
    if let Some(requirement) = fanout_requirement {
        if !system.is_empty() {
            system.push_str("\n\n");
        }
        system.push_str(&fanout_system_instruction(requirement));
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
                                    // Newline separator: fragments must never
                                    // fuse across block boundaries into marker
                                    // text (e.g. `<｜DSML｜` + `tool_calls>`)
                                    // inside replayed reasoning content.
                                    if !reasoning_content.is_empty() {
                                        reasoning_content.push('\n');
                                    }
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
                    // An assistant turn whose blocks were all unconvertible
                    // (e.g. `redacted_thinking`) must not reach upstream as a
                    // content-less message: OpenAI-compatible providers reject
                    // it with an opaque 400 that fails the whole conversation.
                    let assistant_turn_has_payload = !assistant_text.is_empty()
                        || !reasoning_content.is_empty()
                        || !tool_calls.is_empty();
                    if !assistant_turn_has_payload {
                        continue;
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
    let parallel_tool_calls = if fanout_requirement.is_some() {
        None
    } else {
        tools
            .as_ref()
            .filter(|tools: &&Vec<OpenAiTool>| {
                tools
                    .iter()
                    .any(|tool| is_bridge_search_tool(&tool.function.name))
            })
            .map(|_| false)
    };

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FanoutRequirement {
    requested_total: Option<usize>,
    remaining: Option<usize>,
    web_research_requested: bool,
}

fn fanout_system_instruction(requirement: FanoutRequirement) -> String {
    let web_tool_policy = if requirement.web_research_requested {
        "The user explicitly requested online/web research, so WebSearch or WebFetch may be used when needed; limit WebSearch to at most two distinct queries per Agent."
    } else {
        "The user did not explicitly request online/web research. In every Agent prompt explicitly state: Do not call WebSearch or WebFetch; use internal knowledge only."
    };

    match (requirement.requested_total, requirement.remaining) {
        (Some(total), Some(remaining)) => format!(
            "Claude Code compatibility requirement: the user explicitly requested a fan-out of subagents. Before giving a final answer, issue exactly {remaining} additional Agent tool call(s) with distinct, non-overlapping research scopes to satisfy the user's requested total of {total} Agent calls. If more than one call remains, emit as many as possible in the same assistant turn so Claude Code can execute them concurrently. Set run_in_background=true on every required Agent call. You must launch all requested Agent calls before calling TaskOutput, waiting for any Agent to complete, or synthesizing findings. If the model emits only part of the requested fan-out in one assistant turn, immediately issue the remaining Agent call(s) in the next turn while the already-launched background Agents continue running; do not wait for their results first. In every Agent prompt, require a direct structured final report and explicitly forbid that subagent from spawning Agent children or requesting Bash, Read, TaskOutput, or other unavailable tools. {web_tool_policy} Do not replace the required Agent calls with parent-level WebSearch or WebFetch. Do not cap the fan-out at two when the user requested more. A response with fewer than {total} total Agent calls is incomplete. Only after all {total} Agent calls have been launched, collect their results with TaskOutput (or completion notifications) and synthesize the combined findings immediately without launching more agents."
        ),
        _ => format!(
            "Claude Code compatibility requirement: the user explicitly requested a fan-out of subagents. Before giving a final answer, issue multiple Agent tool calls with distinct, non-overlapping research scopes. Use the number of agents/scopes explicitly requested by the user; if no count was specified, choose a practical number based on the independent scopes in the request. If more than one call is needed, emit as many as possible in the same assistant turn so Claude Code can execute them concurrently. Set run_in_background=true on every required Agent call. You must launch all requested Agent calls before calling TaskOutput, waiting for any Agent to complete, or synthesizing findings. If only part of the intended fan-out is emitted in one assistant turn, immediately launch the remaining Agent call(s) in the next turn while the already-launched background Agents continue running; do not wait for their results first. In every Agent prompt, require a direct structured final report and explicitly forbid that subagent from spawning Agent children or requesting Bash, Read, TaskOutput, or other unavailable tools. {web_tool_policy} Do not replace the required Agent calls with parent-level WebSearch or WebFetch. Do not cap the fan-out at two when the user requested more. Only after the intended Agent fan-out has been fully launched, collect their results with TaskOutput (or completion notifications) and synthesize the combined findings immediately without launching unnecessary extra agents."
        ),
    }
}

fn explicit_fanout_requirement(payload: &MessagesRequest) -> Option<FanoutRequirement> {
    if !has_agent_tool(payload) {
        return None;
    }

    let mut user_text = String::new();
    for message in &payload.messages {
        if message.role != "user" {
            continue;
        }
        let text = message_content_text(&message.content);
        let Some(text) = crate::handlers::strip_leading_system_reminders(&text) else {
            continue;
        };
        user_text.push_str(text);
        user_text.push('\n');
    }

    let normalized = user_text.to_lowercase();
    let web_research_requested = explicit_web_research_requested(&normalized);
    let explicit = [
        "fan sub agent",
        "fan subagent",
        "fan-out subagent",
        "fan out subagent",
        "parallel subagent",
        "multiple subagent",
        "dispatch subagent",
        "dispatch at least",
        "task tool to dispatch",
        "task tool",
        "agent tool call",
        "agent tool calls",
        "agent tool",
        "spawn subagent",
        "launch subagent",
        "nhiều subagent",
        "chia subagent",
        "subagent song song",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
        || (normalized.contains("subagent")
            && ["dispatch", "spawn", "launch", "task tool", "agent tool"]
                .iter()
                .any(|needle| normalized.contains(needle)));
    if !explicit {
        return None;
    }

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
        "do not use agent tool",
        "do not use the agent tool",
        "don't use agent tool",
        "don't use the agent tool",
        "dont use agent tool",
        "dont use the agent tool",
        "no agent tool",
        "not use agent tool",
        "not use the agent tool",
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

    if let Some(requested_total) = requested_subagent_count(&normalized) {
        return (prior_agent_calls < requested_total).then_some(FanoutRequirement {
            requested_total: Some(requested_total),
            remaining: Some(requested_total - prior_agent_calls),
            web_research_requested,
        });
    }

    (prior_agent_calls == 0).then_some(FanoutRequirement {
        requested_total: None,
        remaining: None,
        web_research_requested,
    })
}

fn explicit_web_research_requested(normalized: &str) -> bool {
    let negated = [
        "do not use websearch",
        "do not use web search",
        "don't use websearch",
        "don't use web search",
        "dont use websearch",
        "dont use web search",
        "do not call websearch",
        "do not call web search",
        "don't call websearch",
        "don't call web search",
        "dont call websearch",
        "dont call web search",
        "no websearch",
        "no web search",
        "without websearch",
        "without web search",
        "do not browse the web",
        "don't browse the web",
        "dont browse the web",
        "do not search the web",
        "don't search the web",
        "dont search the web",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    if negated {
        return false;
    }

    [
        "web search",
        "search the web",
        "browse the web",
        "web research",
        "search online",
        "browse online",
        "research online",
        "online research",
        "search the internet",
        "browse the internet",
        "look up online",
        "look up on the web",
        "find sources online",
        "find online sources",
        "use websearch",
        "use web search",
        "@web search",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn has_agent_tool(payload: &MessagesRequest) -> bool {
    payload.tools.as_ref().is_some_and(|tools| {
        tools
            .iter()
            .any(|tool| tool.name.eq_ignore_ascii_case("Agent"))
    })
}

fn requested_subagent_count(normalized: &str) -> Option<usize> {
    let tokens = normalized
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();

    for (index, token) in tokens.iter().enumerate() {
        if !matches!(*token, "subagent" | "subagents" | "agent" | "agents") {
            continue;
        }
        let start = index.saturating_sub(6);
        for candidate in tokens[start..index].iter().rev() {
            if let Some(count) = parse_small_count(candidate) {
                if count >= 2 {
                    return Some(count);
                }
            }
        }
    }
    None
}

fn parse_small_count(token: &str) -> Option<usize> {
    if let Ok(value) = token.parse::<usize>() {
        return (value > 0 && value <= 64).then_some(value);
    }
    match token {
        "two" | "hai" => Some(2),
        "three" | "ba" => Some(3),
        "four" | "bon" | "bốn" | "tu" | "tư" => Some(4),
        "five" | "nam" | "năm" => Some(5),
        "six" | "sau" | "sáu" => Some(6),
        "seven" | "bay" | "bảy" => Some(7),
        "eight" | "tam" | "tám" => Some(8),
        "nine" | "chin" | "chín" => Some(9),
        "ten" | "muoi" | "mười" => Some(10),
        _ => None,
    }
}
