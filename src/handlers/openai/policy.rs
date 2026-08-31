//! OpenAI client policy enforcement and model-specific request normalization.

use crate::api_key::{is_web_search_tool, ApiKeyPolicyError, AuthenticatedClient, ReasoningMode};
use crate::error::BridgeError;
use crate::opencode::mapper::is_deepseek_v4_model;
use crate::opencode::types::OpenAiInboundRequest;
use serde_json::{json, Value};

pub(super) fn apply_openai_client_policy(
    client: &AuthenticatedClient,
    payload: &mut OpenAiInboundRequest,
) -> Result<(), BridgeError> {
    let policy = &client.policy;
    if payload.stream && !policy.permissions.streaming {
        return Err(policy_error(ApiKeyPolicyError::StreamingDisabled));
    }

    // Tool gating must cover both the modern `tools` array and the legacy
    // OpenAI `functions` field: a key without tool (or web-search) permission
    // must not slip capability declarations past the gate by switching wire
    // syntax. Gating keys on *presence* of a non-empty declaration list (the
    // historical `tools` semantic), while the web-search sub-check inspects
    // callable names across both wire shapes.
    let tool_fields = ["tools", "functions"];
    let has_tool_declarations = tool_fields.into_iter().any(|field| {
        payload
            .extra
            .get(field)
            .and_then(Value::as_array)
            .is_some_and(|items| !items.is_empty())
    });
    if has_tool_declarations {
        if !policy.permissions.tools {
            return Err(policy_error(ApiKeyPolicyError::ToolsDisabled));
        }
        let web_search_requested = tool_fields
            .into_iter()
            .filter_map(|field| payload.extra.get(field))
            .filter_map(Value::as_array)
            .flatten()
            .filter_map(openai_tool_name)
            .any(is_web_search_tool);
        if !policy.permissions.web_search && web_search_requested {
            return Err(policy_error(ApiKeyPolicyError::WebSearchDisabled));
        }
    }

    let token_field = if payload.extra.contains_key("max_completion_tokens") {
        "max_completion_tokens"
    } else {
        "max_tokens"
    };
    // Enforce the per-key output cap on *every* token-limit field the client
    // actually sent: a body carrying both `max_tokens` and the newer
    // `max_completion_tokens` must not slip the un-preferred sibling past the
    // clamp. Absent fields stay absent, except that the preferred field is
    // seeded below when the policy defines a cap and neither was requested.
    let any_token_field_present = ["max_completion_tokens", "max_tokens"]
        .into_iter()
        .any(|field| payload.extra.contains_key(field));
    for field in ["max_completion_tokens", "max_tokens"] {
        let requested = payload
            .extra
            .get(field)
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok());
        if requested.is_none()
            && (!payload.extra.contains_key(field) || policy.max_output_tokens.is_none())
        {
            continue;
        }
        if let Some(enforced) = policy
            .enforce_output_tokens(requested)
            .map_err(policy_error)?
        {
            payload.extra.insert(field.to_string(), json!(enforced));
        }
    }
    if !any_token_field_present && policy.max_output_tokens.is_some() {
        if let Some(enforced) = policy.enforce_output_tokens(None).map_err(policy_error)? {
            payload
                .extra
                .insert(token_field.to_string(), json!(enforced));
        }
    }

    match policy.reasoning_mode {
        ReasoningMode::Disabled => {
            payload
                .extra
                .insert("thinking".to_string(), json!({"type":"disabled"}));
            payload.extra.remove("reasoning_effort");
        }
        ReasoningMode::Enabled => {
            let requested_budget = payload
                .extra
                .get("thinking")
                .and_then(Value::as_object)
                .and_then(|thinking| thinking.get("budget_tokens"))
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok());
            let budget = policy
                .enforce_reasoning_tokens(requested_budget)
                .map_err(policy_error)?;
            let mut thinking = serde_json::Map::new();
            thinking.insert("type".to_string(), json!("enabled"));
            if let Some(budget) = budget {
                thinking.insert("budget_tokens".to_string(), json!(budget));
            }
            payload
                .extra
                .insert("thinking".to_string(), Value::Object(thinking));
            if let Some(effort) = &policy.reasoning_effort {
                payload
                    .extra
                    .insert("reasoning_effort".to_string(), json!(effort));
            }
        }
        ReasoningMode::Inherit => {
            let thinking_enabled = payload
                .extra
                .get("thinking")
                .and_then(Value::as_object)
                .and_then(|thinking| thinking.get("type"))
                .and_then(Value::as_str)
                .is_some_and(|value| matches!(value, "enabled" | "adaptive"));
            if thinking_enabled && policy.max_reasoning_tokens.is_some() {
                let requested = payload
                    .extra
                    .get("thinking")
                    .and_then(Value::as_object)
                    .and_then(|thinking| thinking.get("budget_tokens"))
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok());
                let enforced = policy
                    .enforce_reasoning_tokens(requested)
                    .map_err(policy_error)?;
                if let Some(enforced) = enforced {
                    if let Some(thinking) = payload
                        .extra
                        .get_mut("thinking")
                        .and_then(Value::as_object_mut)
                    {
                        thinking.insert("budget_tokens".to_string(), json!(enforced));
                    }
                }
            }
        }
    }

    Ok(())
}

pub(super) fn policy_error(error: ApiKeyPolicyError) -> BridgeError {
    BridgeError::Forbidden(error.to_string())
}

/// Extract the callable name from either wire shape: modern
/// `{"type":"function","function":{"name":…}}` or legacy `{"name":…}`.
fn openai_tool_name(tool: &Value) -> Option<&str> {
    tool.get("function")
        .and_then(|function| function.get("name"))
        .or_else(|| tool.get("name"))
        .and_then(Value::as_str)
}

pub(super) fn normalize_openai_request_for_model(payload: &mut OpenAiInboundRequest) {
    if !is_deepseek_v4_model(&payload.model) {
        return;
    }

    let explicit_thinking = payload
        .extra
        .get("thinking")
        .and_then(serde_json::Value::as_object)
        .and_then(|object| object.get("type"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    let has_reasoning_effort = payload
        .extra
        .get("reasoning_effort")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|effort| !effort.trim().is_empty());

    if explicit_thinking.is_none() {
        payload.extra.insert(
            "thinking".to_string(),
            serde_json::json!({
                "type": if has_reasoning_effort { "enabled" } else { "disabled" }
            }),
        );
    }

    let thinking_enabled = explicit_thinking
        .as_deref()
        .map(|value| matches!(value, "enabled" | "adaptive"))
        .unwrap_or(has_reasoning_effort);
    if thinking_enabled {
        // DeepSeek V4 rejects or ignores these sampling/tool-choice controls in
        // reasoning mode. Keep the tools themselves and let the model select.
        payload.extra.remove("tool_choice");
        payload.extra.remove("temperature");
        payload.extra.remove("top_p");
    }
}
