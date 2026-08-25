//! OpenAI-compatible `/v1/chat/completions` transport.

use crate::api_key::{is_web_search_tool, ApiKeyPolicyError, AuthenticatedClient, ReasoningMode};
use crate::config::DEFAULT_MODEL;
use crate::error::BridgeError;
use crate::history::{HistoryCapture, HistoryRequestStart};
use crate::observability::RequestId;
use crate::opencode::mapper::{is_deepseek_v4_model, map_model_name};
use crate::opencode::retry::execute_openai_with_warp_retry;
use crate::opencode::types::OpenAiInboundRequest;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const HISTORY_CONVERSATION_HEADER: &str = "x-opencode-history-conversation-id";
const HISTORY_PARENT_HEADER: &str = "x-opencode-history-parent-request-id";
const HISTORY_OPERATION_HEADER: &str = "x-opencode-history-operation";

fn history_header(headers: &HeaderMap, name: &'static str) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?.trim();
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return None;
    }
    Some(value.to_string())
}

pub async fn handle_chat_completions(
    State(state): State<AppState>,
    client: Option<Extension<AuthenticatedClient>>,
    request_id: Option<Extension<RequestId>>,
    headers: HeaderMap,
    payload: Result<Json<OpenAiInboundRequest>, JsonRejection>,
) -> Response {
    match handle_chat_completions_inner(
        state,
        client.map(|Extension(value)| value),
        request_id.map(|Extension(value)| value),
        headers,
        payload,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => openai_bridge_error(error),
    }
}

async fn handle_chat_completions_inner(
    state: AppState,
    client: Option<AuthenticatedClient>,
    request_id: Option<RequestId>,
    headers: HeaderMap,
    payload: Result<Json<OpenAiInboundRequest>, JsonRejection>,
) -> Result<Response, BridgeError> {
    let Json(mut payload) = payload.map_err(|error| {
        BridgeError::InvalidRequest(format!("Invalid OpenAI request body: {error}"))
    })?;
    let inbound_request = serde_json::to_value(&payload).ok();
    let requested_model = (!payload.model.trim().is_empty()).then(|| payload.model.clone());

    if payload.messages.is_empty() {
        return Err(BridgeError::InvalidRequest(
            "messages must contain at least one item".to_string(),
        ));
    }

    let _permit =
        match &state.rate_limiter {
            Some(limiter) => Some(limiter.acquire().await.map_err(|_| {
                BridgeError::InvalidRequest("Rate limiter is unavailable".to_string())
            })?),
            None => None,
        };

    if let Some(client) = &client {
        apply_openai_client_policy(client, &mut payload)?;
    }

    let selected_model = match &client {
        Some(client) => client
            .policy
            .resolve_model(
                (!payload.model.trim().is_empty()).then_some(payload.model.as_str()),
                state.config.model.as_deref(),
                DEFAULT_MODEL,
            )
            .map_err(policy_error)?,
        None => state
            .config
            .model
            .clone()
            .or_else(|| (!payload.model.trim().is_empty()).then(|| payload.model.clone()))
            .ok_or_else(|| BridgeError::InvalidRequest("model is required".to_string()))?,
    };
    payload.model = map_model_name(&selected_model);
    normalize_openai_request_for_model(&mut payload);

    let thinking_requested = payload
        .extra
        .get("thinking")
        .and_then(Value::as_object)
        .and_then(|thinking| thinking.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|value| matches!(value, "enabled" | "adaptive"))
        || payload
            .extra
            .get("include_reasoning")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || payload.extra.contains_key("reasoning_effort");
    let reasoning_effort = payload
        .extra
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let reasoning_budget_tokens = payload
        .extra
        .get("thinking")
        .and_then(Value::as_object)
        .and_then(|thinking| thinking.get("budget_tokens"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    let history_request_id = request_id
        .map(|value| value.0)
        .unwrap_or_else(|| format!("history-openai-{}", crate::history::now_ms()));
    let conversation_id = history_header(&headers, HISTORY_CONVERSATION_HEADER);
    let parent_request_id = history_header(&headers, HISTORY_PARENT_HEADER);
    let operation_kind = match history_header(&headers, HISTORY_OPERATION_HEADER).as_deref() {
        Some("response_recovery") if parent_request_id.is_some() => "response_recovery",
        Some("model_test") => "model_test",
        _ => "chat_completions",
    };
    let capture = state.history.begin(HistoryRequestStart {
        id: history_request_id,
        conversation_id,
        parent_request_id,
        protocol: "openai".to_string(),
        endpoint: "/v1/chat/completions".to_string(),
        operation_kind: operation_kind.to_string(),
        client_key_id: client.as_ref().map(|value| value.key_id.clone()),
        client_name: client.as_ref().map(|value| value.name.clone()),
        client_environment: client.as_ref().map(|value| value.environment.clone()),
        requested_model,
        effective_model: Some(selected_model.clone()),
        stream: payload.stream,
        thinking_requested,
        reasoning_effort,
        reasoning_budget_tokens,
        inbound: inbound_request,
    });
    if let Ok(value) = serde_json::to_value(&payload) {
        capture.effective_json(&value, Some(&payload.model), "primary", 1);
    }

    let routing_key = client
        .as_ref()
        .map(|client| client.key_id.as_str())
        .or_else(|| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
        })
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
        })
        .unwrap_or("default-agent");

    let is_stream = payload.stream;
    let upstream = match execute_openai_with_warp_retry(&state, routing_key, &payload).await {
        Ok(response) => response,
        Err(error) => {
            capture.attempt_finished(
                None,
                "failed",
                None,
                Some("transport_or_provider_error"),
                Some(&error.to_string()),
            );
            capture.fail(None, "forward_error", &error.to_string());
            return Err(error);
        }
    };
    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let max_capture_bytes = state.config.history.max_response_bytes;
    let body_stream = async_stream::stream! {
        let mut upstream_stream = upstream.bytes_stream();
        let mut collector = OpenAiResponseCollector::new(
            capture,
            is_stream,
            status,
            max_capture_bytes,
        );
        while let Some(chunk_result) = upstream_stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    collector.push(&chunk);
                    yield Ok::<Bytes, std::io::Error>(chunk);
                }
                Err(error) => {
                    collector.fail("upstream_body_error", &error.to_string());
                    yield Err(std::io::Error::other(format!("upstream body error: {error}")));
                    return;
                }
            }
        }
        collector.finish();
    };

    let mut builder = Response::builder().status(status).header(
        header::CONTENT_TYPE,
        if is_stream {
            "text/event-stream"
        } else {
            "application/json"
        },
    );
    if is_stream {
        builder = builder
            .header(header::CACHE_CONTROL, "no-cache")
            .header("x-accel-buffering", "no");
    }
    builder
        .body(Body::from_stream(body_stream))
        .map_err(|error| BridgeError::UpstreamError(format!("response build failed: {error}")))
}

struct OpenAiResponseCollector {
    capture: HistoryCapture,
    is_stream: bool,
    status: StatusCode,
    max_bytes: usize,
    buffer: Vec<u8>,
    finished: bool,
    first_chunk: bool,
}

#[derive(Default)]
struct ParsedOpenAiHistory {
    model: Option<String>,
    finish_reason: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
}

impl OpenAiResponseCollector {
    fn new(capture: HistoryCapture, is_stream: bool, status: StatusCode, max_bytes: usize) -> Self {
        Self {
            capture,
            is_stream,
            status,
            max_bytes: max_bytes.max(1024),
            buffer: Vec::new(),
            finished: false,
            first_chunk: false,
        }
    }

    fn push(&mut self, chunk: &Bytes) {
        if !self.first_chunk {
            self.capture.first_chunk();
            self.first_chunk = true;
        }
        if self.buffer.len() >= self.max_bytes {
            return;
        }
        let remaining = self.max_bytes - self.buffer.len();
        self.buffer
            .extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }

    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let parsed = self.parse_buffer();
        self.capture.usage(
            parsed.input_tokens,
            parsed.output_tokens,
            parsed.reasoning_tokens,
        );
        self.capture.response_model(parsed.model.as_deref());
        if self.status.is_success() {
            self.capture.attempt_finished(
                Some(self.status.as_u16()),
                "completed",
                parsed.finish_reason.as_deref(),
                None,
                None,
            );
            self.capture.finish_success(
                self.status.as_u16(),
                parsed.finish_reason.as_deref(),
                parsed.model.as_deref(),
            );
        } else {
            self.capture.attempt_finished(
                Some(self.status.as_u16()),
                "failed",
                parsed.finish_reason.as_deref(),
                Some("upstream_non_2xx"),
                Some(&format!("upstream returned status {}", self.status)),
            );
            self.capture.fail(
                Some(self.status.as_u16()),
                "upstream_non_2xx",
                &format!("upstream returned status {}", self.status),
            );
        }
    }

    fn fail(&mut self, error_type: &str, message: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        let _ = self.parse_buffer();
        self.capture.attempt_finished(
            Some(self.status.as_u16()),
            "failed",
            None,
            Some(error_type),
            Some(message),
        );
        self.capture
            .fail(Some(self.status.as_u16()), error_type, message);
    }

    fn parse_buffer(&self) -> ParsedOpenAiHistory {
        let raw = String::from_utf8_lossy(&self.buffer);
        self.capture.provider_raw_response(&raw);
        if self.is_stream {
            parse_openai_sse_history(&raw, &self.capture)
        } else {
            parse_openai_sync_history(&self.buffer, &self.capture)
        }
    }
}

impl Drop for OpenAiResponseCollector {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let _ = self.parse_buffer();
        self.capture.attempt_finished(
            Some(self.status.as_u16()),
            "cancelled",
            None,
            Some("client_cancelled"),
            Some("client stopped reading the OpenAI response stream"),
        );
        self.capture.cancel();
        self.finished = true;
    }
}

fn parse_openai_sync_history(body: &[u8], capture: &HistoryCapture) -> ParsedOpenAiHistory {
    let Ok(root) = serde_json::from_slice::<Value>(body) else {
        return ParsedOpenAiHistory::default();
    };
    let mut parsed = ParsedOpenAiHistory {
        model: root
            .get("model")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        ..Default::default()
    };
    if let Some(usage) = root.get("usage") {
        parsed.input_tokens = usage
            .get("prompt_tokens")
            .or_else(|| usage.get("input_tokens"))
            .and_then(Value::as_u64);
        parsed.output_tokens = usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens"))
            .and_then(Value::as_u64);
        parsed.reasoning_tokens = usage
            .get("completion_tokens_details")
            .and_then(|details| details.get("reasoning_tokens"))
            .or_else(|| usage.get("reasoning_tokens"))
            .and_then(Value::as_u64);
    }
    if let Some(choice) = root
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    {
        parsed.finish_reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        if let Some(message) = choice.get("message") {
            if let Some(reasoning) = message
                .get("reasoning_content")
                .or_else(|| message.get("reasoning"))
                .or_else(|| message.get("thinking"))
                .and_then(Value::as_str)
            {
                capture.append_reasoning(reasoning);
            }
            if let Some(content) = message.get("content") {
                append_openai_content(content, capture);
            }
            capture_openai_tool_calls(message.get("tool_calls"), capture);
        }
    }
    parsed
}

fn parse_openai_sse_history(raw: &str, capture: &HistoryCapture) -> ParsedOpenAiHistory {
    let mut parsed = ParsedOpenAiHistory::default();
    let mut tool_calls = BTreeMap::<usize, (String, String)>::new();
    for line in raw.lines() {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        if parsed.model.is_none() {
            parsed.model = event
                .get("model")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
        }
        if let Some(usage) = event.get("usage") {
            parsed.input_tokens = usage
                .get("prompt_tokens")
                .or_else(|| usage.get("input_tokens"))
                .and_then(Value::as_u64)
                .or(parsed.input_tokens);
            parsed.output_tokens = usage
                .get("completion_tokens")
                .or_else(|| usage.get("output_tokens"))
                .and_then(Value::as_u64)
                .or(parsed.output_tokens);
            parsed.reasoning_tokens = usage
                .get("completion_tokens_details")
                .and_then(|details| details.get("reasoning_tokens"))
                .or_else(|| usage.get("reasoning_tokens"))
                .and_then(Value::as_u64)
                .or(parsed.reasoning_tokens);
        }
        for choice in event
            .get("choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                parsed.finish_reason = Some(reason.to_string());
            }
            let Some(delta) = choice.get("delta") else {
                continue;
            };
            if let Some(reasoning) = delta
                .get("reasoning_content")
                .or_else(|| delta.get("reasoning"))
                .or_else(|| delta.get("thinking"))
                .and_then(Value::as_str)
            {
                capture.append_reasoning(reasoning);
            }
            if let Some(content) = delta.get("content") {
                append_openai_content(content, capture);
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let entry = tool_calls.entry(index).or_default();
                    if let Some(function) = call.get("function") {
                        if let Some(name) = function.get("name").and_then(Value::as_str) {
                            entry.0.push_str(name);
                        }
                        if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                            entry.1.push_str(arguments);
                        }
                    }
                }
            }
        }
    }
    for (_, (name, arguments)) in tool_calls {
        capture.tool_call(
            if name.is_empty() { "tool_call" } else { &name },
            (!arguments.is_empty()).then_some(arguments.as_str()),
        );
    }
    parsed
}

fn append_openai_content(content: &Value, capture: &HistoryCapture) {
    match content {
        Value::String(text) => capture.append_response(text),
        Value::Array(items) => {
            for item in items {
                if let Some(text) = item
                    .get("text")
                    .or_else(|| item.get("content"))
                    .and_then(Value::as_str)
                {
                    capture.append_response(text);
                }
            }
        }
        Value::Null => {}
        other => capture.append_response(&other.to_string()),
    }
}

fn capture_openai_tool_calls(calls: Option<&Value>, capture: &HistoryCapture) {
    let Some(calls) = calls.and_then(Value::as_array) else {
        return;
    };
    for call in calls {
        let function = call.get("function").unwrap_or(call);
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("tool_call");
        let arguments = function.get("arguments").and_then(Value::as_str);
        capture.tool_call(name, arguments);
    }
}

fn apply_openai_client_policy(
    client: &AuthenticatedClient,
    payload: &mut OpenAiInboundRequest,
) -> Result<(), BridgeError> {
    let policy = &client.policy;
    if payload.stream && !policy.permissions.streaming {
        return Err(policy_error(ApiKeyPolicyError::StreamingDisabled));
    }

    if let Some(Value::Array(tools)) = payload
        .extra
        .get("tools")
        .filter(|value| value.as_array().is_some_and(|items| !items.is_empty()))
    {
        if !policy.permissions.tools {
            return Err(policy_error(ApiKeyPolicyError::ToolsDisabled));
        }
        if !policy.permissions.web_search
            && tools.iter().any(|tool| {
                tool.get("function")
                    .and_then(|function| function.get("name"))
                    .or_else(|| tool.get("name"))
                    .and_then(Value::as_str)
                    .is_some_and(is_web_search_tool)
            })
        {
            return Err(policy_error(ApiKeyPolicyError::WebSearchDisabled));
        }
    }

    let token_field = if payload.extra.contains_key("max_completion_tokens") {
        "max_completion_tokens"
    } else {
        "max_tokens"
    };
    let requested_tokens = payload
        .extra
        .get(token_field)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    if requested_tokens.is_some() || policy.max_output_tokens.is_some() {
        if let Some(enforced) = policy
            .enforce_output_tokens(requested_tokens)
            .map_err(policy_error)?
        {
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

fn policy_error(error: ApiKeyPolicyError) -> BridgeError {
    BridgeError::Forbidden(error.to_string())
}

fn normalize_openai_request_for_model(payload: &mut OpenAiInboundRequest) {
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

pub fn openai_error_response(
    status: StatusCode,
    error_type: &str,
    code: Option<&str>,
    message: impl Into<String>,
) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "message": message.into(),
                "type": error_type,
                "param": null,
                "code": code,
            }
        })),
    )
        .into_response()
}

fn openai_bridge_error(error: BridgeError) -> Response {
    match error {
        BridgeError::InvalidRequest(message) => openai_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            None,
            message,
        ),
        BridgeError::Unauthorized(message) => openai_error_response(
            StatusCode::UNAUTHORIZED,
            "invalid_request_error",
            Some("invalid_api_key"),
            message,
        ),
        BridgeError::Forbidden(message) => openai_error_response(
            StatusCode::FORBIDDEN,
            "permission_error",
            Some("key_policy_denied"),
            message,
        ),
        BridgeError::RateLimited(message) => openai_error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            Some("rate_limit_exceeded"),
            message,
        ),
        BridgeError::EgressUnavailable(message) => openai_error_response(
            StatusCode::BAD_REQUEST,
            "api_error",
            Some("egress_unavailable"),
            message,
        ),
        other => openai_error_response(
            StatusCode::BAD_GATEWAY,
            "api_error",
            None,
            other.to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_key::{ApiKeyPolicy, LimitAction, ReasoningMode};

    fn client(policy: ApiKeyPolicy) -> AuthenticatedClient {
        AuthenticatedClient {
            key_id: "key_test".to_string(),
            name: "Test".to_string(),
            environment: "development".to_string(),
            policy,
        }
    }

    #[test]
    fn openai_errors_have_expected_shape() {
        let response = openai_error_response(
            StatusCode::UNAUTHORIZED,
            "invalid_request_error",
            Some("invalid_api_key"),
            "bad key",
        );
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn deepseek_openai_defaults_to_disabled_thinking() {
        let mut payload = OpenAiInboundRequest {
            model: "deepseek-v4-flash-free".to_string(),
            messages: vec![serde_json::json!({"role":"user","content":"hi"})],
            stream: false,
            extra: std::collections::BTreeMap::from([(
                "tool_choice".to_string(),
                serde_json::json!({"type":"function","function":{"name":"tool"}}),
            )]),
        };
        normalize_openai_request_for_model(&mut payload);
        assert_eq!(payload.extra["thinking"]["type"], "disabled");
        assert!(payload.extra.contains_key("tool_choice"));
    }

    #[test]
    fn deepseek_reasoning_removes_conflicting_controls() {
        let mut payload = OpenAiInboundRequest {
            model: "deepseek-v4-flash-free".to_string(),
            messages: vec![],
            stream: true,
            extra: std::collections::BTreeMap::from([
                ("reasoning_effort".to_string(), serde_json::json!("max")),
                ("tool_choice".to_string(), serde_json::json!("required")),
                ("temperature".to_string(), serde_json::json!(0.2)),
            ]),
        };
        normalize_openai_request_for_model(&mut payload);
        assert_eq!(payload.extra["thinking"]["type"], "enabled");
        assert!(!payload.extra.contains_key("tool_choice"));
        assert!(!payload.extra.contains_key("temperature"));
    }

    #[test]
    fn per_key_policy_clamps_tokens_and_forces_reasoning() {
        let mut payload = OpenAiInboundRequest {
            model: "opencode/deepseek-v4-flash-free".to_string(),
            messages: vec![json!({"role":"user","content":"hi"})],
            stream: false,
            extra: std::collections::BTreeMap::from([("max_tokens".to_string(), json!(8192))]),
        };
        let policy = ApiKeyPolicy {
            max_output_tokens: Some(4096),
            max_reasoning_tokens: Some(2048),
            limit_action: LimitAction::Clamp,
            reasoning_mode: ReasoningMode::Enabled,
            reasoning_effort: Some("max".to_string()),
            ..Default::default()
        };
        apply_openai_client_policy(&client(policy), &mut payload).unwrap();
        assert_eq!(payload.extra["max_tokens"], 4096);
        assert_eq!(payload.extra["thinking"]["budget_tokens"], 2048);
        assert_eq!(payload.extra["reasoning_effort"], "max");
    }
}
