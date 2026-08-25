//! POST /v1/messages orchestration.

use super::prompt::{extract_prompt, last_user_shell_cmd};
use super::shell;
use super::title;
use super::{MessagesRequest, OutputConfig, ThinkingConfig};
use crate::api_key::{is_web_search_tool, ApiKeyPolicyError, AuthenticatedClient, ReasoningMode};
use crate::config::DEFAULT_MODEL;
use crate::error::BridgeError;
use crate::history::HistoryRequestStart;
use crate::observability::RequestId;
use crate::opencode;
use crate::state::AppState;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use axum::response::sse::{KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::collections::HashSet;
use tracing::info;

pub async fn handle_messages(
    State(state): State<AppState>,
    client: Option<Extension<AuthenticatedClient>>,
    request_id: Option<Extension<RequestId>>,
    headers: HeaderMap,
    payload: Result<Json<MessagesRequest>, JsonRejection>,
) -> Result<Response, BridgeError> {
    let Json(mut payload) = payload
        .map_err(|error| BridgeError::InvalidRequest(format!("Invalid request body: {error}")))?;
    let _rate_permit = acquire_rate_permit(&state).await?;
    validate_messages_request(&payload)?;

    let inbound_request = serde_json::to_value(&payload).ok();
    let requested_model = payload.model.clone();
    let client = client.map(|Extension(value)| value);
    if let Some(client) = &client {
        apply_client_policy(client, &mut payload)?;
    }

    let model = match &client {
        Some(client) => client
            .policy
            .resolve_model(
                payload.model.as_deref(),
                state.config.model.as_deref(),
                DEFAULT_MODEL,
            )
            .map_err(policy_error)?,
        None => state
            .config
            .model
            .clone()
            .or_else(|| payload.model.clone())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
    };
    log_request(&payload, &model, client.as_ref());

    let operation_kind = if last_user_shell_cmd(&payload.messages).is_some() {
        "shell"
    } else {
        "messages"
    };
    let request_id = request_id
        .map(|Extension(value)| value.0)
        .unwrap_or_else(|| format!("history-anthropic-{}", crate::history::now_ms()));
    let capture = state.history.begin(HistoryRequestStart {
        id: request_id,
        conversation_id: None,
        parent_request_id: None,
        protocol: "anthropic".to_string(),
        endpoint: "/v1/messages".to_string(),
        operation_kind: operation_kind.to_string(),
        client_key_id: client.as_ref().map(|value| value.key_id.clone()),
        client_name: client.as_ref().map(|value| value.name.clone()),
        client_environment: client.as_ref().map(|value| value.environment.clone()),
        requested_model,
        effective_model: Some(model.clone()),
        stream: payload.stream,
        thinking_requested: payload.thinking_enabled() == Some(true),
        reasoning_effort: payload.reasoning_effort().map(ToOwned::to_owned),
        reasoning_budget_tokens: payload
            .thinking
            .as_ref()
            .and_then(|thinking| thinking.budget_tokens),
        inbound: inbound_request,
    });

    if let Some(response) = title::try_handle(&payload, model.clone()) {
        capture.finish_success(
            response.status().as_u16(),
            Some("local_title"),
            Some(&model),
        );
        return Ok(response);
    }

    match shell::try_handle(&state, &payload, model.clone()).await {
        Ok(Some(response)) => {
            capture.finish_success(
                response.status().as_u16(),
                Some("local_shell"),
                Some(&model),
            );
            return Ok(response);
        }
        Ok(None) => {}
        Err(error) => {
            capture.fail(None, "shell_error", &error.to_string());
            return Err(error);
        }
    }

    let routing_key = client
        .as_ref()
        .map(|client| client.key_id.clone())
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            headers
                .get("Authorization")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "default-agent".to_string());

    if payload.stream {
        let stream = match opencode::forward_to_llm_stream(
            &state,
            routing_key,
            payload,
            model,
            state.config.channel_capacity,
            state.search_client.clone(),
            state.config.max_search_loops,
            capture.clone(),
        )
        .await
        {
            Ok(stream) => stream,
            Err(error) => {
                capture.fail(None, "forward_error", &error.to_string());
                return Err(error);
            }
        };
        return Ok(disable_proxy_buffering(
            Sse::new(stream)
                .keep_alive(KeepAlive::default())
                .into_response(),
        ));
    }

    let response = match opencode::forward_to_llm_sync(
        &state,
        routing_key,
        payload,
        model,
        state.search_client.clone(),
        state.config.max_search_loops,
        capture.clone(),
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            capture.fail(None, "forward_error", &error.to_string());
            return Err(error);
        }
    };
    Ok(Json(response).into_response())
}

fn validate_messages_request(payload: &MessagesRequest) -> Result<(), BridgeError> {
    if payload.messages.is_empty() {
        return Err(BridgeError::InvalidRequest("No messages found".to_string()));
    }
    if payload.max_tokens == Some(0) {
        return Err(BridgeError::InvalidRequest(
            "max_tokens must be greater than zero".to_string(),
        ));
    }
    if payload
        .temperature
        .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
    {
        return Err(BridgeError::InvalidRequest(
            "temperature must be between 0 and 1".to_string(),
        ));
    }
    if payload
        .top_p
        .is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
    {
        return Err(BridgeError::InvalidRequest(
            "top_p must be between 0 and 1".to_string(),
        ));
    }

    if let Some(system) = &payload.system {
        let valid = system.is_string()
            || system.as_array().is_some_and(|parts| {
                parts.iter().all(|part| {
                    part.get("type").and_then(serde_json::Value::as_str) == Some("text")
                        && part
                            .get("text")
                            .and_then(serde_json::Value::as_str)
                            .is_some()
                })
            });
        if !valid {
            return Err(BridgeError::InvalidRequest(
                "system must be a string or an array of text blocks".to_string(),
            ));
        }
    }

    let mut tool_names = HashSet::new();
    if let Some(tools) = &payload.tools {
        for tool in tools {
            let name = tool.name.trim();
            if name.is_empty() || name.len() > 128 {
                return Err(BridgeError::InvalidRequest(
                    "tool names must contain 1 to 128 bytes".to_string(),
                ));
            }
            if !tool.input_schema.is_object() {
                return Err(BridgeError::InvalidRequest(format!(
                    "input_schema for tool `{name}` must be a JSON object"
                )));
            }
            if !tool_names.insert(name.to_ascii_lowercase()) {
                return Err(BridgeError::InvalidRequest(format!(
                    "duplicate tool name `{name}` is ambiguous"
                )));
            }
        }
    }

    for (message_index, message) in payload.messages.iter().enumerate() {
        if !matches!(message.role.as_str(), "user" | "assistant" | "system") {
            return Err(BridgeError::InvalidRequest(format!(
                "messages[{message_index}].role must be `user`, `assistant`, or `system`"
            )));
        }
        let crate::handlers::ContentVal::Multiple(blocks) = &message.content else {
            continue;
        };
        if blocks.is_empty() {
            return Err(BridgeError::InvalidRequest(format!(
                "messages[{message_index}].content must not be empty"
            )));
        }
        for (block_index, block) in blocks.iter().enumerate() {
            let location = format!("messages[{message_index}].content[{block_index}]");
            if block.content_type.trim().is_empty() {
                return Err(BridgeError::InvalidRequest(format!(
                    "{location}.type must not be empty"
                )));
            }
            if message.role == "system" && block.content_type != "text" {
                return Err(BridgeError::InvalidRequest(format!(
                    "{location} must be a text block in a system message"
                )));
            }
            match block.content_type.as_str() {
                "text" if block.text.is_none() => {
                    return Err(BridgeError::InvalidRequest(format!(
                        "{location}.text is required"
                    )));
                }
                "thinking" if block.thinking.is_none() && block.text.is_none() => {
                    return Err(BridgeError::InvalidRequest(format!(
                        "{location}.thinking is required"
                    )));
                }
                "tool_use" => {
                    if message.role != "assistant" {
                        return Err(BridgeError::InvalidRequest(format!(
                            "{location} is only valid in an assistant message"
                        )));
                    }
                    if block.id.as_deref().is_none_or(str::is_empty)
                        || block.name.as_deref().is_none_or(str::is_empty)
                        || !block
                            .input
                            .as_ref()
                            .is_some_and(serde_json::Value::is_object)
                    {
                        return Err(BridgeError::InvalidRequest(format!(
                            "{location} requires non-empty id/name and an object input"
                        )));
                    }
                }
                "tool_result" => {
                    if message.role != "user" {
                        return Err(BridgeError::InvalidRequest(format!(
                            "{location} is only valid in a user message"
                        )));
                    }
                    if block.tool_use_id.as_deref().is_none_or(str::is_empty)
                        || block.content.is_none()
                    {
                        return Err(BridgeError::InvalidRequest(format!(
                            "{location} requires tool_use_id and content"
                        )));
                    }
                }
                _ => {}
            }
        }
    }

    if let Some(choice) = &payload.tool_choice {
        let selected = if let Some(value) = choice.as_str() {
            match value {
                "auto" | "any" | "required" | "none" => None,
                other => {
                    return Err(BridgeError::InvalidRequest(format!(
                        "unsupported tool_choice `{other}`"
                    )));
                }
            }
        } else if let Some(object) = choice.as_object() {
            match object.get("type").and_then(serde_json::Value::as_str) {
                Some("auto" | "any" | "required" | "none") => None,
                Some("tool") => Some(
                    object
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .filter(|name| !name.is_empty())
                        .ok_or_else(|| {
                            BridgeError::InvalidRequest(
                                "tool_choice type `tool` requires a name".to_string(),
                            )
                        })?,
                ),
                _ => {
                    return Err(BridgeError::InvalidRequest(
                        "tool_choice has an unsupported shape".to_string(),
                    ));
                }
            }
        } else {
            return Err(BridgeError::InvalidRequest(
                "tool_choice must be a string or object".to_string(),
            ));
        };
        if let Some(selected) = selected {
            if !tool_names.contains(&selected.to_ascii_lowercase()) {
                return Err(BridgeError::InvalidRequest(format!(
                    "tool_choice references unavailable tool `{selected}`"
                )));
            }
        }
    }

    Ok(())
}

fn apply_client_policy(
    client: &AuthenticatedClient,
    payload: &mut MessagesRequest,
) -> Result<(), BridgeError> {
    let policy = &client.policy;
    if payload.stream && !policy.permissions.streaming {
        return Err(policy_error(ApiKeyPolicyError::StreamingDisabled));
    }

    if let Some(tools) = payload.tools.as_ref().filter(|tools| !tools.is_empty()) {
        if !policy.permissions.tools {
            return Err(policy_error(ApiKeyPolicyError::ToolsDisabled));
        }
        if !policy.permissions.web_search && tools.iter().any(|tool| is_web_search_tool(&tool.name))
        {
            return Err(policy_error(ApiKeyPolicyError::WebSearchDisabled));
        }
    }

    if last_user_shell_cmd(&payload.messages).is_some() && !policy.permissions.shell {
        return Err(policy_error(ApiKeyPolicyError::ShellDisabled));
    }

    payload.max_tokens = policy
        .enforce_output_tokens(payload.max_tokens)
        .map_err(policy_error)?;

    match policy.reasoning_mode {
        ReasoningMode::Disabled => {
            payload.thinking = Some(ThinkingConfig {
                thinking_type: "disabled".to_string(),
                budget_tokens: None,
                ..Default::default()
            });
            if let Some(output) = &mut payload.output_config {
                output.effort = None;
            }
            payload.extra.remove("reasoning_effort");
        }
        ReasoningMode::Enabled => {
            let requested_budget = payload
                .thinking
                .as_ref()
                .and_then(|thinking| thinking.budget_tokens);
            let budget = policy
                .enforce_reasoning_tokens(requested_budget)
                .map_err(policy_error)?;
            let mut thinking = payload.thinking.take().unwrap_or_default();
            thinking.thinking_type = "enabled".to_string();
            thinking.budget_tokens = budget;
            payload.thinking = Some(thinking);
            if let Some(effort) = &policy.reasoning_effort {
                payload
                    .output_config
                    .get_or_insert_with(OutputConfig::default)
                    .effort = Some(effort.clone());
            }
        }
        ReasoningMode::Inherit => {
            if let Some(thinking) = &mut payload.thinking {
                if thinking.is_enabled() == Some(true) {
                    thinking.budget_tokens = policy
                        .enforce_reasoning_tokens(thinking.budget_tokens)
                        .map_err(policy_error)?;
                }
            }
        }
    }

    Ok(())
}

fn policy_error(error: ApiKeyPolicyError) -> BridgeError {
    BridgeError::Forbidden(error.to_string())
}

pub(super) fn disable_proxy_buffering(mut response: Response) -> Response {
    response.headers_mut().insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    response
}

async fn acquire_rate_permit(
    state: &AppState,
) -> Result<Option<tokio::sync::SemaphorePermit<'_>>, BridgeError> {
    match &state.rate_limiter {
        Some(limiter) => limiter
            .acquire()
            .await
            .map(Some)
            .map_err(|_| BridgeError::InvalidRequest("Rate limiter is unavailable".to_string())),
        None => Ok(None),
    }
}

fn log_request(payload: &MessagesRequest, model: &str, client: Option<&AuthenticatedClient>) {
    let prompt = extract_prompt(&payload.messages);
    info!(
        message_count = payload.messages.len(),
        prompt_chars = prompt.len(),
        client_id = client.map(|value| value.key_id.as_str()).unwrap_or("anonymous"),
        %model,
        "incoming messages request"
    );
    if let Some(tools) = &payload.tools {
        info!(
            tools = ?tools.iter().map(|tool| &tool.name).collect::<Vec<_>>(),
            "client tools available"
        );
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use crate::api_key::{ApiKeyPermissions, ApiKeyPolicy, LimitAction, ReasoningMode};
    use crate::handlers::{ContentVal, Message};

    fn request() -> MessagesRequest {
        MessagesRequest {
            model: Some("opencode/test".to_string()),
            messages: vec![Message {
                role: "user".to_string(),
                content: ContentVal::Single("hello".to_string()),
            }],
            max_tokens: Some(8192),
            ..Default::default()
        }
    }

    fn client(policy: ApiKeyPolicy) -> AuthenticatedClient {
        AuthenticatedClient {
            key_id: "key_test".to_string(),
            name: "Test".to_string(),
            environment: "development".to_string(),
            policy,
        }
    }

    #[test]
    fn policy_clamps_output_and_enables_reasoning() {
        let mut payload = request();
        let policy = ApiKeyPolicy {
            max_output_tokens: Some(4096),
            max_reasoning_tokens: Some(2048),
            limit_action: LimitAction::Clamp,
            reasoning_mode: ReasoningMode::Enabled,
            reasoning_effort: Some("high".to_string()),
            ..Default::default()
        };
        apply_client_policy(&client(policy), &mut payload).unwrap();
        assert_eq!(payload.max_tokens, Some(4096));
        assert_eq!(
            payload
                .thinking
                .as_ref()
                .and_then(|value| value.budget_tokens),
            Some(2048)
        );
        assert_eq!(payload.reasoning_effort(), Some("high"));
    }

    #[test]
    fn policy_blocks_tools_and_shell() {
        let mut payload = request();
        payload.messages[0].content = ContentVal::Single("!pwd".to_string());
        let policy = ApiKeyPolicy {
            permissions: ApiKeyPermissions {
                shell: false,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(
            apply_client_policy(&client(policy), &mut payload),
            Err(BridgeError::Forbidden(_))
        ));
    }

    #[test]
    fn policy_blocks_shell_after_claude_code_system_reminders() {
        let mut payload = request();
        payload.messages[0].content = ContentVal::Single(
            concat!(
                "<system-reminder>Available tools...</system-reminder>\n",
                "<system-reminder>Available skills...</system-reminder>\n\n",
                "!pwd"
            )
            .to_string(),
        );
        let policy = ApiKeyPolicy {
            permissions: ApiKeyPermissions {
                shell: false,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(
            apply_client_policy(&client(policy), &mut payload),
            Err(BridgeError::Forbidden(_))
        ));
    }
}

#[cfg(test)]
mod request_validation_tests {
    use super::*;
    use crate::handlers::{AnthropicTool, ContentVal, Message, MessageContent};

    fn base_request() -> MessagesRequest {
        MessagesRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: ContentVal::Single("hello".to_string()),
            }],
            max_tokens: Some(128),
            ..Default::default()
        }
    }

    #[test]
    fn rejects_case_insensitive_duplicate_tool_names_and_non_object_schema() {
        let mut request = base_request();
        request.tools = Some(vec![
            AnthropicTool {
                name: "Read".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
            AnthropicTool {
                name: "read".to_string(),
                input_schema: serde_json::json!({"type":"object"}),
                ..Default::default()
            },
        ]);
        assert!(validate_messages_request(&request)
            .unwrap_err()
            .to_string()
            .contains("duplicate tool"));

        request.tools = Some(vec![AnthropicTool {
            name: "Read".to_string(),
            input_schema: serde_json::json!("not-an-object"),
            ..Default::default()
        }]);
        assert!(validate_messages_request(&request)
            .unwrap_err()
            .to_string()
            .contains("input_schema"));
    }

    #[test]
    fn rejects_malformed_tool_history_blocks() {
        let mut request = base_request();
        request.messages = vec![Message {
            role: "assistant".to_string(),
            content: ContentVal::Multiple(vec![MessageContent {
                content_type: "tool_use".to_string(),
                id: Some("call-1".to_string()),
                name: Some("Read".to_string()),
                input: Some(serde_json::json!(["not", "object"])),
                ..Default::default()
            }]),
        }];
        assert!(validate_messages_request(&request)
            .unwrap_err()
            .to_string()
            .contains("object input"));

        request.messages = vec![Message {
            role: "assistant".to_string(),
            content: ContentVal::Multiple(vec![MessageContent {
                content_type: "tool_result".to_string(),
                tool_use_id: Some("call-1".to_string()),
                content: Some(serde_json::json!("result")),
                ..Default::default()
            }]),
        }];
        assert!(validate_messages_request(&request)
            .unwrap_err()
            .to_string()
            .contains("user message"));
    }

    #[test]
    fn accepts_claude_code_system_message_with_text_only() {
        let mut request = base_request();
        request.messages.push(Message {
            role: "system".to_string(),
            content: ContentVal::Multiple(vec![MessageContent {
                content_type: "text".to_string(),
                text: Some("SessionStart hook additional context".to_string()),
                ..Default::default()
            }]),
        });
        assert!(validate_messages_request(&request).is_ok());
    }

    #[test]
    fn rejects_non_text_blocks_in_system_messages() {
        let mut request = base_request();
        request.messages.push(Message {
            role: "system".to_string(),
            content: ContentVal::Multiple(vec![MessageContent {
                content_type: "tool_use".to_string(),
                id: Some("call-1".to_string()),
                name: Some("Read".to_string()),
                input: Some(serde_json::json!({})),
                ..Default::default()
            }]),
        });
        assert!(validate_messages_request(&request)
            .unwrap_err()
            .to_string()
            .contains("text block in a system message"));
    }

    #[test]
    fn rejects_tool_choice_for_unavailable_tool() {
        let mut request = base_request();
        request.tools = Some(vec![AnthropicTool {
            name: "Read".to_string(),
            input_schema: serde_json::json!({"type":"object"}),
            ..Default::default()
        }]);
        request.tool_choice = Some(serde_json::json!({"type":"tool","name":"Write"}));
        assert!(validate_messages_request(&request)
            .unwrap_err()
            .to_string()
            .contains("unavailable tool"));
    }

    #[test]
    fn preserves_future_content_blocks_while_validating_known_protocol_blocks() {
        let mut request = base_request();
        request.messages[0].content = ContentVal::Multiple(vec![MessageContent {
            content_type: "future_multimodal_block".to_string(),
            source: Some(serde_json::json!({"type":"base64","data":"abc"})),
            ..Default::default()
        }]);
        assert!(validate_messages_request(&request).is_ok());
    }
}
