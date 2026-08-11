//! Model- and request-shape policy for upstream request mapping.

use super::helpers::extract_system_prompt;
use crate::handlers::{ContentVal, MessagesRequest};
use crate::opencode::types::OpenAiThinkingConfig;
use serde_json::{json, Value};

pub(super) const DEFAULT_MIN_REASONING_STREAM_TOKENS: u32 = 1024;

pub fn is_deepseek_v4_model(model: &str) -> bool {
    let name = model.to_ascii_lowercase();
    name.contains("deepseek-v4-flash") || name.contains("deepseek-v4-pro")
}

pub fn is_deepseek_v4_flash_free_model(model: &str) -> bool {
    model
        .to_ascii_lowercase()
        .contains("deepseek-v4-flash-free")
}

pub(super) fn is_reasoning_heavy_model(model: &str) -> bool {
    let name = model.to_ascii_lowercase();
    is_deepseek_v4_model(&name)
        || (name.contains("deepseek") && (name.contains("r1") || name.contains("reasoner")))
        || name.contains("reasoning")
        || name.contains("-r1")
}

pub fn is_compact_request(payload: &MessagesRequest) -> bool {
    if let Some(system_val) = &payload.system {
        let system_str = extract_system_prompt(system_val).to_lowercase();
        if system_str.contains("compact") || system_str.contains("summari") {
            return true;
        }
    }
    for msg in &payload.messages {
        match &msg.content {
            ContentVal::Single(text) => {
                let text_lower = text.to_lowercase();
                if text_lower.contains("compact") || text_lower.contains("summari") {
                    return true;
                }
            }
            ContentVal::Multiple(blocks) => {
                for block in blocks {
                    if block.content_type == "text" {
                        if let Some(text) = &block.text {
                            let text_lower = text.to_lowercase();
                            if text_lower.contains("compact") || text_lower.contains("summari") {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

pub(super) fn normalize_upstream_thinking(
    payload: &MessagesRequest,
    mapped_model: &str,
) -> Option<OpenAiThinkingConfig> {
    if !is_deepseek_v4_model(mapped_model) {
        return None;
    }

    // Anthropic semantics: absence means no extended thinking. DeepSeek V4 defaults
    // to thinking enabled, so an absent Claude field must be made explicit upstream.
    let thinking_type = match payload.thinking_enabled() {
        Some(true) => "enabled",
        Some(false) | None => "disabled",
    };
    Some(OpenAiThinkingConfig {
        thinking_type: thinking_type.to_string(),
    })
}

pub(super) fn normalize_reasoning_effort(
    payload: &MessagesRequest,
    mapped_model: &str,
    upstream_thinking: Option<&OpenAiThinkingConfig>,
) -> Option<String> {
    if upstream_thinking.is_some_and(|config| config.thinking_type == "disabled") {
        return None;
    }

    let effort = payload.reasoning_effort()?.trim().to_ascii_lowercase();
    if effort.is_empty() {
        return None;
    }

    if is_deepseek_v4_model(mapped_model) {
        return match effort.as_str() {
            "low" | "medium" | "high" => Some("high".to_string()),
            "xhigh" | "max" | "ultracode" => Some("max".to_string()),
            _ => None,
        };
    }

    Some(effort)
}

pub(super) fn include_reasoning_for_stream(
    stream: bool,
    mapped_model: &str,
    is_compact: bool,
    explicit_thinking: Option<bool>,
) -> Option<bool> {
    if explicit_thinking == Some(false) {
        return Some(false);
    }
    if is_compact {
        return None;
    }
    if explicit_thinking == Some(true) {
        return Some(true);
    }
    if stream && is_reasoning_heavy_model(mapped_model) {
        Some(true)
    } else {
        None
    }
}

pub(super) fn normalize_response_format(
    payload: &MessagesRequest,
    mapped_model: &str,
) -> Option<Value> {
    let format = payload.output_config.as_ref()?.format.as_ref()?;
    let format_type = format.get("type").and_then(Value::as_str)?;

    // The free DFLASH backend rejects every response_format variant as
    // grammar-constrained decoding. The schema cannot be forwarded upstream;
    // [`dropped_schema_system_instruction`] preserves it in the system prompt
    // instead.
    if is_deepseek_v4_flash_free_model(mapped_model) {
        return None;
    }

    match format_type {
        "json_object" => Some(json!({"type": "json_object"})),
        "json_schema" if is_deepseek_v4_model(mapped_model) => {
            // DeepSeek Chat Completions documents JSON object mode, while Claude's
            // structured-output request carries a JSON schema.
            Some(json!({"type": "json_object"}))
        }
        "json_schema" => {
            let schema = format.get("schema")?.clone();
            Some(json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "claude_code_output",
                    "schema": schema,
                    "strict": true
                }
            }))
        }
        _ => None,
    }
}

/// System-prompt instruction that preserves a structured-output schema for
/// free DFLASH, whose upstream `response_format` is dropped by
/// [`normalize_response_format`]: the schema must still reach the model.
pub(super) fn dropped_schema_system_instruction(
    payload: &MessagesRequest,
    mapped_model: &str,
) -> Option<String> {
    if !is_deepseek_v4_flash_free_model(mapped_model) {
        return None;
    }
    let format = payload.output_config.as_ref()?.format.as_ref()?;
    if format.get("type").and_then(Value::as_str)? != "json_schema" {
        return None;
    }
    let schema = format.get("schema")?;
    Some(format!(
        "The user requested structured output. Return exactly one JSON object matching this JSON schema and no other text:\n{}",
        serde_json::to_string_pretty(schema).ok()?
    ))
}

pub(super) fn normalize_upstream_max_tokens(
    requested: Option<u32>,
    stream: bool,
    mapped_model: &str,
    is_compact: bool,
    minimum_reasoning_stream_tokens: u32,
) -> Option<u32> {
    if is_compact {
        return requested;
    }
    if !stream || !is_reasoning_heavy_model(mapped_model) {
        return requested;
    }

    let floor = minimum_reasoning_stream_tokens.max(1);
    Some(requested.map(|value| value.max(floor)).unwrap_or(floor))
}
