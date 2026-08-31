//! Search-result injection and search-related helpers for the streaming
//! and synchronous forwarding paths.

use crate::handlers::{ContentVal, MessagesRequest};
use crate::opencode::mapper::{extract_search_query, is_web_search_tool};

pub(crate) fn inject_search_results(
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

pub(crate) fn resolve_search_query(tool_args: &str, payload: &MessagesRequest) -> (String, bool) {
    let extracted = extract_search_query(tool_args);
    if !extracted.trim().is_empty() {
        return (bound_search_query(&extracted), false);
    }
    let fallback = latest_user_text(payload)
        .map(|text| bound_search_query(&text))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "current user request".to_string());
    (fallback, true)
}

pub(crate) fn normalize_search_query(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(crate) fn prepare_native_tool_retry(payload: &mut MessagesRequest) {
    let available_tools = payload
        .tools
        .as_ref()
        .map(|tools| {
            tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|names| !names.is_empty())
        .unwrap_or_else(|| "none".to_string());
    append_system_instruction(
        payload,
        &format!(
            "Your previous response expressed an intended tool call as encoded text instead of using the API's native tool-calling protocol. Re-evaluate the current task and reissue every intended tool invocation using native function/tool calls only. Use only tools available in this request: {available_tools}. Every tool argument payload must be one complete JSON object matching the supplied tool schema. Do not print `[Requesting ...]` markers, DSML, `<tool_calls>`, `<tvToolcalls>`, XML tool markup, or prose that merely describes a tool call. Invoke the native tool now if a tool is still required; otherwise answer normally."
        ),
    );
}

pub(crate) fn prepare_compat_tool_retry(payload: &mut MessagesRequest) {
    let available_tools = payload
        .tools
        .as_ref()
        .map(|tools| {
            tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|names| !names.is_empty())
        .unwrap_or_else(|| "none".to_string());
    append_system_instruction(
        payload,
        &format!(
            "Your previous response attempted a tool call using an unsupported, incomplete, ambiguous, batched-search, or prose-only marker. Re-evaluate the current task and reissue every intended tool call using the exact compatibility form `[Requesting Tool execution: 'ToolName' with arguments: {{complete JSON object}}]`. Emit exactly one marker per invocation; never place comma-separated argument objects in one marker. Use only tools available in this request: {available_tools}. Do not use `Requesting Tool invocation`, `Requesting tool call(s) for ...`, `with parameters`, `Write file at`, positional function syntax, prose placeholders, omitted arguments, or tool markers inside code blocks. For Write, include both `file_path` and the complete `content`. Emit real tool calls now instead of describing them."
        ),
    );
}

pub(crate) fn prepare_final_search_synthesis(payload: &mut MessagesRequest, reason: &str) {
    if let Some(tools) = payload.tools.as_mut() {
        tools.retain(|tool| !is_web_search_tool(&tool.name));
        if tools.is_empty() {
            payload.tools = None;
        }
    }
    payload.tool_choice = None;
    append_system_instruction(
        payload,
        &format!(
            "Web research is complete ({reason}). Do not call WebSearch or WebFetch again. Use the search tool results already present in the conversation and provide the best complete final answer now. If some evidence is missing, state the limitation instead of requesting another search."
        ),
    );
}

pub(crate) fn search_results_with_instruction(results: &str, final_turn: bool) -> String {
    if final_turn {
        format!(
            "{results}\n\n[Bridge instruction: Search budget is complete. Synthesize the final answer from these and all earlier results; do not call WebSearch or WebFetch again.]"
        )
    } else {
        results.to_string()
    }
}

fn latest_user_text(payload: &MessagesRequest) -> Option<String> {
    payload.messages.iter().rev().find_map(|message| {
        if message.role != "user" {
            return None;
        }
        match &message.content {
            ContentVal::Single(text) => non_empty_text(text),
            ContentVal::Multiple(blocks) => {
                let text = blocks
                    .iter()
                    .filter(|block| block.content_type == "text")
                    .filter_map(|block| block.text.as_deref())
                    .collect::<Vec<_>>()
                    .join(" ");
                non_empty_text(&text)
            }
        }
    })
}

fn non_empty_text(text: &str) -> Option<String> {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

fn bound_search_query(query: &str) -> String {
    let normalized = query.split_whitespace().collect::<Vec<_>>().join(" ");
    normalized.chars().take(512).collect()
}

fn append_system_instruction(payload: &mut MessagesRequest, instruction: &str) {
    match payload.system.as_mut() {
        Some(serde_json::Value::String(existing)) => {
            if !existing.is_empty() {
                existing.push_str("\n\n");
            }
            existing.push_str(instruction);
        }
        Some(serde_json::Value::Array(parts)) => parts.push(serde_json::json!({
            "type": "text",
            "text": instruction
        })),
        Some(other) => {
            let previous = other.clone();
            *other = serde_json::json!([
                {"type":"text","text":previous.to_string()},
                {"type":"text","text":instruction}
            ]);
        }
        None => payload.system = Some(serde_json::Value::String(instruction.to_string())),
    }
}
