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
use futures_util::StreamExt;
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
    // Cheap request validation runs before admission so malformed requests
    // never consume a concurrency slot. Live bridge-issued shell tickets are
    // valid external referents for compact client-side/deferred tool-result
    // echoes; forged/expired IDs are not exempted.
    let live_shell_tool_use_ids =
        shell::live_shell_result_ticket_ids(&payload, &state.shell_delegations);
    validate_messages_request_with_known_tool_use_ids(&payload, &live_shell_tool_use_ids)?;

    // Per-key policy and model resolution are equally cheap, pure decisions:
    // they also run before admission so a request whose fate is already
    // decided fails fast with its policy error instead of queueing on the
    // semaphore behind saturated upstream streams.
    let inbound_request = serde_json::to_value(&payload).ok();
    let requested_model = payload.model.clone();
    let client = client.map(|Extension(value)| value);
    if let Some(client) = &client {
        apply_client_policy(client, &mut payload)?;
    }

    let model = match &client {
        Some(client) if client.key_id == "system_claude_code" => resolve_anonymous_model(
            state.config.model.as_deref(),
            payload.model.as_deref(),
            &state.config.retry.upstream_base_url,
        ),
        Some(client) => client
            .policy
            .resolve_model(
                payload.model.as_deref(),
                state.config.model.as_deref(),
                DEFAULT_MODEL,
            )
            .map_err(policy_error)?,
        None => resolve_anonymous_model(
            state.config.model.as_deref(),
            payload.model.as_deref(),
            &state.config.retry.upstream_base_url,
        ),
    };

    // Shell-policy verdicts are decidable pre-admission: reject before queueing on the permit.
    if let Some(rejection) = shell::shell_admission_rejection(
        &state.config.shell_policy,
        client.as_ref(),
        &payload,
        &state.shell_delegations,
    ) {
        return Err(rejection);
    }

    // Acquire an *owned* permit so it can be moved into the response-body
    // stream: the global concurrency limit must cover the whole upstream
    // exchange (including streaming), not just handler setup. Released on
    // early error returns by ordinary drop, and when the body completes or
    // the client disconnects mid-stream.
    let rate_permit = acquire_rate_permit(&state).await?;

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

    match shell::try_handle(&state, client.as_ref(), &payload, model.clone()).await {
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
        // Hold the global rate-limit permit until the body stream is fully
        // consumed or dropped (client disconnect mid-stream included): the
        // permit is moved out of the handler frame into the stream body.
        let stream = async_stream::stream! {
            let _rate_permit = rate_permit;
            let mut stream = std::pin::pin!(stream);
            while let Some(event) = stream.next().await {
                yield event;
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

#[cfg(test)]
fn validate_messages_request(payload: &MessagesRequest) -> Result<(), BridgeError> {
    validate_messages_request_with_known_tool_use_ids(payload, &[])
}

fn validate_messages_request_with_known_tool_use_ids(
    payload: &MessagesRequest,
    externally_known_tool_use_ids: &[String],
) -> Result<(), BridgeError> {
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

    // Anthropic tool_result blocks may only answer a tool_use that has
    // already appeared in an earlier assistant turn. Track IDs in request
    // order so orphan/forward references fail in one linear pass. Duplicate
    // tool_use IDs intentionally keep the first occurrence canonical.
    let mut seen_tool_use_ids: HashSet<String> =
        externally_known_tool_use_ids.iter().cloned().collect();
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
                    // First occurrence is canonical; duplicate IDs are kept
                    // compatible with historical Claude Code transcripts.
                    seen_tool_use_ids.insert(block.id.as_deref().unwrap().to_string());
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
                    let tool_use_id = block.tool_use_id.as_deref().unwrap();
                    if !seen_tool_use_ids.contains(tool_use_id) {
                        return Err(BridgeError::InvalidRequest(format!(
                            "{location} references unknown prior tool_use_id `{tool_use_id}`"
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
) -> Result<Option<tokio::sync::OwnedSemaphorePermit>, BridgeError> {
    match &state.rate_limiter {
        Some(limiter) => limiter
            .clone()
            .acquire_owned()
            .await
            .map(Some)
            .map_err(|_| BridgeError::InvalidRequest("Rate limiter is unavailable".to_string())),
        None => Ok(None),
    }
}

/// Model selection for anonymous (auth-disabled) callers.
/// Maps Claude Code model aliases (e.g. from `/model`) to upstream model tiers:
/// - Opus / 1M models -> 1M group (glm-5.3-flash on b.ai, opencode/x-preview-f-free on OpenCode)
/// - Sonnet / Haiku / standard models -> Sub-1M group (qwen3.8-flash on b.ai, mimo-v2.5-free on OpenCode)
/// - Curated direct matches -> preserved
/// - Other/unspecified -> configured global model or fallback
fn resolve_anonymous_model(
    configured: Option<&str>,
    requested: Option<&str>,
    upstream_base_url: &str,
) -> String {
    let configured_clean = configured.map(str::trim).filter(|value| !value.is_empty());
    let requested_clean = requested.map(str::trim).filter(|value| !value.is_empty());

    let is_opencode = crate::application::prober::is_opencode_upstream(upstream_base_url)
        || configured_clean.is_some_and(|c| c.starts_with("opencode/"));

    if let Some(req) = requested_clean {
        let req_lower = req.to_ascii_lowercase();

        // Check for Claude family aliases (from Claude Code CLI /model selection)
        if req_lower.contains("opus") || req_lower.contains("1m") {
            if is_opencode {
                if let Some(cfg) = configured_clean {
                    if crate::application::models::resolve_model_profile(cfg).context_window
                        >= 1_000_000
                    {
                        return cfg.to_string();
                    }
                }
                return "opencode/x-preview-f-free".to_string();
            } else {
                if let Some(cfg) = configured_clean {
                    if crate::application::models::resolve_model_profile(cfg).context_window
                        >= 1_000_000
                    {
                        return cfg.to_string();
                    }
                }
                return "glm-5.3-flash".to_string();
            }
        }

        if req_lower.contains("sonnet") || req_lower.contains("haiku") {
            if is_opencode {
                if let Some(cfg) = configured_clean {
                    if crate::application::models::resolve_model_profile(cfg).context_window
                        < 1_000_000
                    {
                        return cfg.to_string();
                    }
                }
                return "opencode/mimo-v2.5-free".to_string();
            } else {
                if let Some(cfg) = configured_clean {
                    if crate::application::models::resolve_model_profile(cfg).context_window
                        < 1_000_000
                    {
                        return cfg.to_string();
                    }
                }
                return "qwen3.8-flash".to_string();
            }
        }

        // Exact match with known curated models
        if is_opencode {
            if crate::application::models::is_supported_free_model(req) {
                return req.to_string();
            }
        } else {
            for p in crate::application::models::API_MODEL_PROFILES {
                if p.id.eq_ignore_ascii_case(req) {
                    return p.id.to_string();
                }
            }
        }

        // Fallback for custom or unknown model: configured wins if present
        configured_clean.unwrap_or(req).to_string()
    } else {
        configured_clean.unwrap_or(DEFAULT_MODEL).to_string()
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
mod rate_limit_tests {
    use crate::config::{BridgeConfig, EgressConfig, EgressMode};
    use crate::server::build_router;
    use crate::state::AppState;
    use axum::body::Body;
    use axum::http::header;
    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::Router;
    use futures_util::StreamExt;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tower::util::ServiceExt;

    async fn sse_upstream() -> axum::response::Response {
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(
                "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n",
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn rate_limit_permit_is_held_for_the_whole_response_body() {
        let upstream = Router::new()
            .route("/chat/completions", post(|| async { sse_upstream().await }))
            .into_make_service();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

        let defaults = BridgeConfig::default();
        let mut config = BridgeConfig {
            model: Some("fixture-model".to_string()),
            retry: crate::config::RetryConfig {
                upstream_base_url: format!("http://{address}"),
                max_network_attempts: 1,
                base_backoff: Duration::ZERO,
                ..defaults.retry
            },
            egress: EgressConfig {
                mode: EgressMode::Direct,
                ..defaults.egress
            },
            ..defaults
        };
        config.observability.max_concurrent_requests = Some(1);
        config.management.config_path = std::env::temp_dir().join(format!(
            "opencode2api-messages-ratelimit-{}-{}.toml",
            std::process::id(),
            crate::api_key::unix_timestamp(),
        ));
        let state = AppState::new(config);
        let app = build_router(state.clone());

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "model": "fixture-model",
                            "stream": true,
                            "max_tokens": 64,
                            "messages": [{"role": "user", "content": "hi"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // The response head has returned but the body is still unconsumed:
        // the single global permit must remain held for the whole body.
        let limiter = state.rate_limiter.as_ref().unwrap();
        assert_eq!(
            limiter.available_permits(),
            0,
            "permit must stay acquired while the streaming body is in flight"
        );

        let mut body = response.into_body().into_data_stream();
        while let Some(chunk) = body.next().await {
            chunk.unwrap();
        }
        assert_eq!(
            limiter.available_permits(),
            1,
            "permit must be released once the body completes"
        );
    }
}

#[cfg(test)]
mod admission_order_tests {
    use super::*;
    use crate::api_key::{ApiKeyPermissions, ApiKeyPolicy};
    use crate::config::{BridgeConfig, EgressConfig, EgressMode};
    use axum::body::Body;
    use axum::extract::Extension;
    use axum::http::{header, StatusCode};
    use axum::routing::post;
    use axum::Router;
    use std::time::Duration;
    use tower::util::ServiceExt;

    fn streaming_denied_client() -> AuthenticatedClient {
        AuthenticatedClient {
            key_id: "key_streaming_disabled".to_string(),
            name: "Streaming Disabled".to_string(),
            environment: "development".to_string(),
            policy: ApiKeyPolicy {
                permissions: ApiKeyPermissions {
                    streaming: false,
                    ..Default::default()
                },
                ..Default::default()
            },
        }
    }

    /// A policy-doomed request must fail fast instead of queueing behind a
    /// saturated semaphore: every cheap rejectable condition is decided
    /// before admission, so the caller gets its 403 even while the single
    /// global slot is held elsewhere.
    #[tokio::test]
    async fn policy_rejections_do_not_queue_for_a_concurrency_slot() {
        // Upstream that would hang forever if the doomed request ever
        // reached forwarding; the assertion below proves it never does.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);

        let defaults = BridgeConfig::default();
        let mut config = BridgeConfig {
            model: Some("fixture-model".to_string()),
            retry: crate::config::RetryConfig {
                upstream_base_url: format!("http://{address}"),
                max_network_attempts: 1,
                base_backoff: Duration::ZERO,
                ..defaults.retry
            },
            egress: EgressConfig {
                mode: EgressMode::Direct,
                ..defaults.egress
            },
            ..defaults
        };
        config.observability.max_concurrent_requests = Some(1);
        config.management.config_path = std::env::temp_dir().join(format!(
            "opencode2api-messages-admission-{}-{}.toml",
            std::process::id(),
            crate::api_key::unix_timestamp(),
        ));
        let state = AppState::new(config);

        // Drain the only global slot before the request arrives.
        let held = state
            .rate_limiter
            .as_ref()
            .unwrap()
            .clone()
            .acquire_owned()
            .await
            .unwrap();

        let app = Router::new()
            .route("/v1/messages", post(handle_messages))
            .layer(Extension(streaming_denied_client()))
            .with_state(state);

        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "stream": true,
                    "max_tokens": 64,
                    "messages": [{"role": "user", "content": "hi"}]
                })
                .to_string(),
            ))
            .unwrap();

        let response = tokio::time::timeout(Duration::from_secs(5), app.oneshot(request))
            .await
            .expect("policy-rejected request must not wait for a concurrency slot")
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "streaming-disabled key must be rejected without needing a slot"
        );
        drop(held);
    }
}

#[cfg(test)]
mod model_resolution_tests {
    use super::*;

    #[test]
    fn configured_global_model_still_wins_over_requested() {
        assert_eq!(
            resolve_anonymous_model(
                Some("global-model"),
                Some("requested-model"),
                "https://opencode.ai/zen/v1"
            ),
            "global-model"
        );
    }

    #[test]
    fn requested_model_is_used_when_nothing_is_configured() {
        assert_eq!(
            resolve_anonymous_model(None, Some("requested-model"), "https://opencode.ai/zen/v1"),
            "requested-model"
        );
    }

    #[test]
    fn blank_models_fall_back_instead_of_reaching_upstream() {
        assert_eq!(
            resolve_anonymous_model(None, Some("   "), "https://opencode.ai/zen/v1"),
            DEFAULT_MODEL,
            "whitespace-only requested model must fall back, not forward garbage"
        );
        assert_eq!(
            resolve_anonymous_model(
                Some(""),
                Some("requested-model"),
                "https://opencode.ai/zen/v1"
            ),
            "requested-model",
            "blank configured model must be treated as unset"
        );
        assert_eq!(
            resolve_anonymous_model(Some("  "), None, "https://opencode.ai/zen/v1"),
            DEFAULT_MODEL,
            "blank configured model with nothing else must use the default"
        );
    }

    #[test]
    fn claude_model_command_routes_to_respective_tiers() {
        let b_ai = "https://api.b.ai/v1";
        // Opus / 1M mapping on b.ai
        assert_eq!(
            resolve_anonymous_model(Some("glm-5.3-flash"), Some("claude-opus-5"), b_ai),
            "glm-5.3-flash"
        );
        assert_eq!(
            resolve_anonymous_model(Some("glm-5.3-flash"), Some("claude-3-opus-20240229"), b_ai),
            "glm-5.3-flash"
        );

        // Sonnet & Haiku -> sub-1M (qwen3.8-flash) on b.ai
        assert_eq!(
            resolve_anonymous_model(
                Some("glm-5.3-flash"),
                Some("claude-3-7-sonnet-20250219"),
                b_ai
            ),
            "qwen3.8-flash"
        );
        assert_eq!(
            resolve_anonymous_model(Some("glm-5.3-flash"), Some("claude-sonnet-5"), b_ai),
            "qwen3.8-flash"
        );
        assert_eq!(
            resolve_anonymous_model(
                Some("glm-5.3-flash"),
                Some("claude-3-5-haiku-20241022"),
                b_ai
            ),
            "qwen3.8-flash"
        );

        // OpenCode zen routing
        let zen = "https://opencode.ai/zen/v1";
        assert_eq!(
            resolve_anonymous_model(
                Some("opencode/x-preview-f-free"),
                Some("claude-opus-5"),
                zen
            ),
            "opencode/x-preview-f-free"
        );
        assert_eq!(
            resolve_anonymous_model(
                Some("opencode/x-preview-f-free"),
                Some("claude-3-7-sonnet-20250219"),
                zen
            ),
            "opencode/mimo-v2.5-free"
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

    fn tool_use_block(id: &str) -> MessageContent {
        MessageContent {
            content_type: "tool_use".to_string(),
            id: Some(id.to_string()),
            name: Some("Read".to_string()),
            input: Some(serde_json::json!({"path": "src/lib.rs"})),
            ..Default::default()
        }
    }

    fn tool_result_block(id: &str) -> MessageContent {
        MessageContent {
            content_type: "tool_result".to_string(),
            tool_use_id: Some(id.to_string()),
            content: Some(serde_json::json!("ok")),
            ..Default::default()
        }
    }

    #[test]
    fn rejects_tool_result_referencing_unknown_tool_use_id() {
        let mut request = base_request();
        request.messages = vec![Message {
            role: "user".to_string(),
            content: ContentVal::Multiple(vec![tool_result_block("call-orphan")]),
        }];
        let error = validate_messages_request(&request).unwrap_err().to_string();
        assert!(error.contains("unknown"), "got: {error}");
        assert!(error.contains("call-orphan"), "got: {error}");
    }

    #[test]
    fn accepts_paired_tool_use_and_result_history() {
        let mut request = base_request();
        request.messages = vec![
            Message {
                role: "user".to_string(),
                content: ContentVal::Single("read the file".to_string()),
            },
            Message {
                role: "assistant".to_string(),
                content: ContentVal::Multiple(vec![tool_use_block("call-1")]),
            },
            Message {
                role: "user".to_string(),
                content: ContentVal::Multiple(vec![
                    tool_result_block("call-1"),
                    MessageContent {
                        content_type: "text".to_string(),
                        text: Some("and summarize".to_string()),
                        ..Default::default()
                    },
                ]),
            },
        ];
        assert!(validate_messages_request(&request).is_ok());
    }

    #[test]
    fn rejects_tool_result_that_precedes_its_tool_use() {
        // Anthropic semantics: a tool_result answers the PREVIOUS assistant
        // turn. A forward reference (result before the emitting assistant
        // message) must be rejected like an orphan.
        let mut request = base_request();
        request.messages = vec![
            Message {
                role: "user".to_string(),
                content: ContentVal::Multiple(vec![tool_result_block("call-late")]),
            },
            Message {
                role: "assistant".to_string(),
                content: ContentVal::Multiple(vec![tool_use_block("call-late")]),
            },
        ];
        let error = validate_messages_request(&request).unwrap_err().to_string();
        assert!(error.contains("unknown"), "got: {error}");
    }

    #[test]
    fn duplicate_tool_use_ids_allow_first_reference_match() {
        // Documented judgment call: duplicate tool_use ids are tolerated and
        // the FIRST occurrence is the canonical referent, so a tool_result
        // matching the shared id resolves instead of being rejected as
        // ambiguous.
        let mut request = base_request();
        request.messages = vec![
            Message {
                role: "assistant".to_string(),
                content: ContentVal::Multiple(vec![tool_use_block("call-dup")]),
            },
            Message {
                role: "assistant".to_string(),
                content: ContentVal::Multiple(vec![tool_use_block("call-dup")]),
            },
            Message {
                role: "user".to_string(),
                content: ContentVal::Multiple(vec![tool_result_block("call-dup")]),
            },
        ];
        assert!(validate_messages_request(&request).is_ok());
    }

    #[test]
    fn rejects_empty_tool_use_ids_at_both_ends() {
        let mut request = base_request();
        request.messages = vec![
            Message {
                role: "assistant".to_string(),
                content: ContentVal::Multiple(vec![tool_use_block("")]),
            },
            Message {
                role: "user".to_string(),
                content: ContentVal::Multiple(vec![tool_result_block("call-1")]),
            },
        ];
        assert!(validate_messages_request(&request)
            .unwrap_err()
            .to_string()
            .contains("non-empty id"));

        request.messages = vec![Message {
            role: "user".to_string(),
            content: ContentVal::Multiple(vec![tool_result_block("")]),
        }];
        assert!(validate_messages_request(&request)
            .unwrap_err()
            .to_string()
            .contains("requires tool_use_id"));
    }

    #[test]
    fn validates_large_paired_histories_in_a_single_pass() {
        let mut request = base_request();
        let mut messages = Vec::with_capacity(4_001);
        messages.push(Message {
            role: "user".to_string(),
            content: ContentVal::Single("go".to_string()),
        });
        for round in 0..2_000_u32 {
            let id = format!("call-{round}");
            messages.push(Message {
                role: "assistant".to_string(),
                content: ContentVal::Multiple(vec![tool_use_block(&id)]),
            });
            messages.push(Message {
                role: "user".to_string(),
                content: ContentVal::Multiple(vec![tool_result_block(&id)]),
            });
        }
        request.messages = messages;
        assert!(validate_messages_request(&request).is_ok());
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
