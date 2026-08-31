//! OpenAI-compatible `/v1/chat/completions` transport.

mod capture;
mod error;
mod policy;

pub use error::openai_error_response;

use capture::OpenAiResponseCollector;
use error::openai_bridge_error;
use policy::{apply_openai_client_policy, normalize_openai_request_for_model, policy_error};

use crate::api_key::AuthenticatedClient;
use crate::config::DEFAULT_MODEL;
use crate::error::BridgeError;
use crate::history::HistoryRequestStart;
use crate::observability::RequestId;
use crate::opencode::mapper::map_model_name;
use crate::opencode::retry::execute_openai_with_warp_retry;
use crate::opencode::types::OpenAiInboundRequest;
use crate::state::AppState;
use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, State};
use axum::http::{header, HeaderMap, StatusCode};
// The following are used only by the inline test modules through
// `use super::*`; gated on cfg(test) so non-test builds see no unused import.
#[cfg(test)]
use axum::response::IntoResponse;
use axum::response::Response;
use axum::Json;
use bytes::Bytes;
use futures_util::StreamExt;
#[cfg(test)]
use serde_json::json;
use serde_json::Value;
#[cfg(test)]
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

    // Acquire an *owned* permit so it can be moved into the response-body
    // stream: the global concurrency limit must cover the whole upstream
    // exchange (including streaming), not just handler setup. Released on
    // early error returns by ordinary drop, and when the body completes or
    // the client disconnects mid-stream.
    let rate_permit =
        match &state.rate_limiter {
            Some(limiter) => Some(limiter.clone().acquire_owned().await.map_err(|_| {
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
    payload.model = if crate::opencode::mapper::uses_opencode_model_aliases(
        &state.config.retry.upstream_base_url,
    ) {
        map_model_name(&selected_model)
    } else {
        selected_model.clone()
    };
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
        // Hold the global rate-limit permit until the body stream is fully
        // consumed or dropped (client disconnect mid-stream included).
        let _rate_permit = rate_permit;
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
#[cfg(test)]
mod rate_limit_tests {
    use super::*;
    use crate::config::{BridgeConfig, EgressConfig, EgressMode};
    use crate::server::build_router;
    use crate::state::AppState;
    use axum::body::Body;
    use axum::http::header;
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
            "opencode2api-openai-ratelimit-{}-{}.toml",
            std::process::id(),
            crate::api_key::unix_timestamp(),
        ));
        let state = AppState::new(config);
        let app = build_router(state.clone());

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "model": "fixture-model",
                            "stream": true,
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

    async fn json_upstream() -> axum::response::Response {
        axum::response::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "id": "chatcmpl-1",
                    "model": "fixture-model",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "hi"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 3, "completion_tokens": 1}
                })
                .to_string(),
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn rate_limit_permit_is_held_for_the_whole_non_streaming_body() {
        let upstream = Router::new()
            .route(
                "/chat/completions",
                post(|| async { json_upstream().await }),
            )
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
            "opencode2api-openai-ratelimit-sync-{}-{}.toml",
            std::process::id(),
            crate::api_key::unix_timestamp(),
        ));
        let state = AppState::new(config);
        let app = build_router(state.clone());

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "model": "fixture-model",
                            "stream": false,
                            "messages": [{"role": "user", "content": "hi"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Non-streaming responses relay through the same body stream: the
        // single global permit must stay held until the client drains it.
        let limiter = state.rate_limiter.as_ref().unwrap();
        assert_eq!(
            limiter.available_permits(),
            0,
            "permit must stay acquired while the sync response body is in flight"
        );

        let mut body = response.into_body().into_data_stream();
        while let Some(chunk) = body.next().await {
            chunk.unwrap();
        }
        assert_eq!(
            limiter.available_permits(),
            1,
            "permit must be released once the sync body completes"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_key::{ApiKeyPermissions, ApiKeyPolicy, LimitAction, ReasoningMode};

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

    #[tokio::test]
    async fn payment_required_maps_to_402_with_openai_envelope() {
        let response = openai_bridge_error(BridgeError::PaymentRequired(
            "Upstream API requires payment.".to_string(),
        ));
        assert_eq!(
            response.status(),
            StatusCode::PAYMENT_REQUIRED,
            "a billing failure must not be masked as a transient 502"
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["error"]["type"], "billing_error");
        assert_eq!(value["error"]["code"], "payment_required");
    }

    #[test]
    fn legacy_functions_field_is_subject_to_tool_policy() {
        let mut payload = OpenAiInboundRequest {
            model: "fixture-model".to_string(),
            messages: vec![json!({"role":"user","content":"hi"})],
            stream: false,
            extra: std::collections::BTreeMap::from([(
                "functions".to_string(),
                json!([{"name": "run_code", "parameters": {}}]),
            )]),
        };
        let policy = ApiKeyPolicy {
            permissions: ApiKeyPermissions {
                tools: false,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(
            matches!(
                apply_openai_client_policy(&client(policy), &mut payload),
                Err(BridgeError::Forbidden(_))
            ),
            "legacy `functions` must not bypass the tools permission gate"
        );
    }

    #[test]
    fn legacy_functions_web_search_is_gated_by_web_search_permission() {
        let mut payload = OpenAiInboundRequest {
            model: "fixture-model".to_string(),
            messages: vec![json!({"role":"user","content":"hi"})],
            stream: false,
            extra: std::collections::BTreeMap::from([(
                "functions".to_string(),
                json!([{"name": "web_search", "parameters": {}}]),
            )]),
        };
        let policy = ApiKeyPolicy {
            permissions: ApiKeyPermissions {
                web_search: false,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(
            matches!(
                apply_openai_client_policy(&client(policy), &mut payload),
                Err(BridgeError::Forbidden(_))
            ),
            "legacy `functions` must not bypass the web-search permission gate"
        );
    }

    #[test]
    fn requests_without_tool_fields_stay_admissible_under_disabled_tools() {
        let mut payload = OpenAiInboundRequest {
            model: "fixture-model".to_string(),
            messages: vec![json!({"role":"user","content":"hi"})],
            stream: false,
            extra: std::collections::BTreeMap::new(),
        };
        let policy = ApiKeyPolicy {
            permissions: ApiKeyPermissions {
                tools: false,
                web_search: false,
                ..Default::default()
            },
            ..Default::default()
        };
        apply_openai_client_policy(&client(policy), &mut payload)
            .expect("tool gating must stay keyed on tool fields being present");
    }

    #[test]
    fn unnamed_tool_declarations_stay_gated_by_tools_permission() {
        // Historical behavior: any non-empty `tools` array is gated even when
        // no entry carries an extractable name.
        let mut payload = OpenAiInboundRequest {
            model: "fixture-model".to_string(),
            messages: vec![json!({"role":"user","content":"hi"})],
            stream: false,
            extra: std::collections::BTreeMap::from([(
                "tools".to_string(),
                json!([{"type": "function"}]),
            )]),
        };
        let policy = ApiKeyPolicy {
            permissions: ApiKeyPermissions {
                tools: false,
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(
            apply_openai_client_policy(&client(policy), &mut payload),
            Err(BridgeError::Forbidden(_))
        ));
    }

    #[test]
    fn both_token_limit_fields_are_clamped() {
        let mut payload = OpenAiInboundRequest {
            model: "fixture-model".to_string(),
            messages: vec![json!({"role":"user","content":"hi"})],
            stream: false,
            extra: std::collections::BTreeMap::from([
                ("max_completion_tokens".to_string(), json!(10)),
                ("max_tokens".to_string(), json!(99_000)),
            ]),
        };
        let policy = ApiKeyPolicy {
            max_output_tokens: Some(1024),
            limit_action: LimitAction::Clamp,
            ..Default::default()
        };
        apply_openai_client_policy(&client(policy), &mut payload).unwrap();
        assert_eq!(payload.extra["max_completion_tokens"], 10);
        assert_eq!(
            payload.extra["max_tokens"], 1024,
            "the sibling token field must not slip past the output cap"
        );
    }

    // --- Allowlist namespace parity (end-to-end for this entry) ------------

    async fn allowlist_echo_upstream(Json(payload): Json<Value>) -> Response {
        let model = payload["model"].as_str().unwrap_or_default().to_string();
        (
            StatusCode::OK,
            Json(json!({
                "id": "chatcmpl-echo",
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": model},
                    "finish_reason": "stop",
                }],
            })),
        )
            .into_response()
    }

    #[tokio::test]
    async fn resolved_namespace_allowlist_admits_wire_name_and_preserves_custom_wire_model() {
        use crate::config::{BridgeConfig, EgressConfig, EgressMode};
        use axum::routing::post;
        use axum::Router;
        use std::time::Duration;
        use tokio::net::TcpListener;

        // Stub upstream echoes back the model id it actually received.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let upstream = Router::new()
                .route("/chat/completions", post(allowlist_echo_upstream))
                .into_make_service();
            axum::serve(listener, upstream).await.unwrap();
        });

        let defaults = BridgeConfig::default();
        let mut config = BridgeConfig {
            model: None,
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
        config.management.config_path = std::env::temp_dir().join(format!(
            "opencode2api-openai-allowlist-{}-{}.toml",
            std::process::id(),
            crate::api_key::unix_timestamp(),
        ));
        let state = AppState::new(config);

        // The key allows the RESOLVED id only; the client sends the WIRE name.
        let mut policy = ApiKeyPolicy {
            allowed_models: vec!["deepseek-v4-flash-free".to_string()],
            ..Default::default()
        };
        policy.normalize();

        let response = handle_chat_completions_inner(
            state,
            Some(client(policy)),
            None,
            HeaderMap::new(),
            Ok(Json(OpenAiInboundRequest {
                model: "deepseek-v4-flash".to_string(),
                messages: vec![json!({"role": "user", "content": "hi"})],
                stream: false,
                extra: BTreeMap::new(),
            })),
        )
        .await
        .expect("a wire-name request resolving onto the allowlisted id must pass policy");

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body["choices"][0]["message"]["content"], "deepseek-v4-flash",
            "policy may resolve aliases internally, but a custom provider must receive the exact wire model id"
        );
    }
}
