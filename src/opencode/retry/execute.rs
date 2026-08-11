//! Upstream request execution with model fallback and egress-aware retries.

use super::policy::{
    bounded_backoff, build_model_retry_list, classify_reqwest_error, classify_status,
    client_retry_after, is_rate_limit_body, is_reasoning_heavy_model, parse_retry_after,
    FailureClass,
};
use super::response::LeasedResponse;
use crate::error::BridgeError;
use crate::observability::RetryMetricClass;
use crate::opencode::types::{OpenAiInboundRequest, OpenAiRequest};
use crate::proxy_pool::EgressLease;
use crate::state::AppState;
use reqwest::{Client, StatusCode};
use serde::Serialize;
use std::time::Duration;
use tracing::{info, warn};

struct SelectedRoute {
    client: Client,
    proxy_url: Option<String>,
    proxy_index: Option<usize>,
    lease: Option<EgressLease>,
}

trait RetryableOpenAiRequest: Serialize + Clone {
    fn model(&self) -> &str;
    fn stream(&self) -> bool;
    fn set_model(&mut self, model: String);
    fn repair_missing_tool_reasoning(&mut self) -> bool;
    fn disable_reasoning_compatibility(&mut self) -> bool;
    fn strip_response_format(&mut self) -> bool;
}

impl RetryableOpenAiRequest for OpenAiRequest {
    fn model(&self) -> &str {
        &self.model
    }

    fn stream(&self) -> bool {
        self.stream
    }

    fn set_model(&mut self, model: String) {
        self.model = model;
    }

    fn repair_missing_tool_reasoning(&mut self) -> bool {
        let mut changed = false;
        for message in &mut self.messages {
            let has_tool_calls = message
                .tool_calls
                .as_ref()
                .is_some_and(|tool_calls| !tool_calls.is_empty());
            let missing_reasoning = message
                .reasoning_content
                .as_deref()
                .is_none_or(str::is_empty);
            if message.role == "assistant" && has_tool_calls && missing_reasoning {
                message.reasoning_content = Some("Tool call continuation.".to_string());
                changed = true;
            }
        }
        changed
    }

    fn disable_reasoning_compatibility(&mut self) -> bool {
        let mut changed = false;
        if self
            .thinking
            .as_ref()
            .is_none_or(|thinking| thinking.thinking_type != "disabled")
        {
            self.thinking = Some(crate::opencode::types::OpenAiThinkingConfig {
                thinking_type: "disabled".to_string(),
            });
            changed = true;
        }
        changed |= self.reasoning_effort.take().is_some();
        if self.include_reasoning != Some(false) {
            self.include_reasoning = Some(false);
            changed = true;
        }
        for message in &mut self.messages {
            changed |= message.reasoning_content.take().is_some();
        }
        changed
    }

    fn strip_response_format(&mut self) -> bool {
        self.response_format.take().is_some()
    }
}

impl RetryableOpenAiRequest for OpenAiInboundRequest {
    fn model(&self) -> &str {
        &self.model
    }

    fn stream(&self) -> bool {
        self.stream
    }

    fn set_model(&mut self, model: String) {
        self.model = model;
    }

    fn repair_missing_tool_reasoning(&mut self) -> bool {
        let mut changed = false;
        for message in &mut self.messages {
            let Some(object) = message.as_object_mut() else {
                continue;
            };
            let is_assistant =
                object.get("role").and_then(serde_json::Value::as_str) == Some("assistant");
            let has_tool_calls = object
                .get("tool_calls")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|tool_calls| !tool_calls.is_empty());
            let missing_reasoning = object
                .get("reasoning_content")
                .and_then(serde_json::Value::as_str)
                .is_none_or(str::is_empty);
            if is_assistant && has_tool_calls && missing_reasoning {
                object.insert(
                    "reasoning_content".to_string(),
                    serde_json::Value::String("Tool call continuation.".to_string()),
                );
                changed = true;
            }
        }
        changed
    }

    fn disable_reasoning_compatibility(&mut self) -> bool {
        let mut changed = false;
        let disabled = serde_json::json!({"type":"disabled"});
        if self.extra.get("thinking") != Some(&disabled) {
            self.extra.insert("thinking".to_string(), disabled);
            changed = true;
        }
        changed |= self.extra.remove("reasoning_effort").is_some();
        changed |= self.extra.remove("include_reasoning").is_some();
        for message in &mut self.messages {
            if let Some(object) = message.as_object_mut() {
                for field in ["reasoning_content", "reasoning", "thinking"] {
                    changed |= object.remove(field).is_some();
                }
            }
        }
        changed
    }

    fn strip_response_format(&mut self) -> bool {
        self.extra.remove("response_format").is_some()
    }
}

pub(crate) async fn execute_with_warp_retry(
    state: &AppState,
    routing_key: &str,
    request: &OpenAiRequest,
) -> Result<LeasedResponse, BridgeError> {
    execute_retryable_request(state, routing_key, request).await
}

pub(crate) async fn execute_openai_with_warp_retry(
    state: &AppState,
    routing_key: &str,
    request: &OpenAiInboundRequest,
) -> Result<LeasedResponse, BridgeError> {
    execute_retryable_request(state, routing_key, request).await
}

/// Bound contradictory request sanitizations: repair and strip are inverse on
/// reasoning_content, so a persistent 400 must not re-sanitize without a cap.
const MAX_COMPAT_SANITIZE_ROUNDS: u32 = 2;

async fn execute_retryable_request<T: RetryableOpenAiRequest>(
    state: &AppState,
    routing_key: &str,
    request: &T,
) -> Result<LeasedResponse, BridgeError> {
    let max_retries = state.config.retry.max_network_attempts as u32;
    let models = build_model_retry_list(request.model(), request.stream(), &state.config.retry);
    let upstream_url = format!(
        "{}/chat/completions",
        state.config.retry.upstream_base_url.trim_end_matches('/')
    );

    if request.stream() && is_reasoning_heavy_model(request.model()) && models.len() == 1 {
        info!(
            model = %request.model(),
            "preserving reasoning-stream semantics without implicit fallback"
        );
    }

    let mut model_index = 0usize;
    let mut retry_count = 0u32;
    let mut compat_sanitize_rounds = 0u32;
    let mut last_failed_proxy = None;
    let mut compatible_request = request.clone();

    loop {
        let current_model = models
            .get(model_index)
            .cloned()
            .unwrap_or_else(|| request.model().to_string());
        let mut attempt_request = compatible_request.clone();
        attempt_request.set_model(current_model.clone());

        let mut route = select_route(state, routing_key, last_failed_proxy).await?;
        let result = route
            .client
            .post(&upstream_url)
            .json(&attempt_request)
            .send()
            .await;

        match result {
            Ok(response) => {
                record_transport_success(state, route.proxy_index).await;
                let status = response.status();

                if classify_status(status, None) == FailureClass::RateLimit {
                    if advance_model(
                        state,
                        &models,
                        &mut model_index,
                        &current_model,
                        status.as_str(),
                    ) {
                        retry_count = 0;
                        last_failed_proxy = None;
                        continue;
                    }

                    if retry_count < max_retries {
                        state.metrics.record_retry(RetryMetricClass::RateLimit);
                        retry_count += 1;
                        let retry_after = response
                            .headers()
                            .get("retry-after")
                            .and_then(|value| value.to_str().ok())
                            .and_then(|value| {
                                parse_retry_after(value, std::time::SystemTime::now())
                            });
                        apply_rate_limit_penalty(state, &route, retry_count, retry_after).await;
                        last_failed_proxy = None;
                        sleep_backoff(state, retry_count, route.proxy_index).await;
                        continue;
                    }

                    return Err(BridgeError::UpstreamError(format!(
                        "Rate limited after {retry_count} retries (status {status})"
                    )));
                }

                if classify_status(status, None) == FailureClass::ProviderServer {
                    if advance_model(
                        state,
                        &models,
                        &mut model_index,
                        &current_model,
                        status.as_str(),
                    ) {
                        retry_count = 0;
                        last_failed_proxy = None;
                        continue;
                    }

                    // Server errors are transient on the provider side, not on
                    // this bridge: sleep-backoff retrying here multiplies with
                    // the client's own retry loop and produces a minutes-long
                    // "Retrying …" hang. Fail fast at the API error so the
                    // client's single retry succeeds immediately against a
                    // recovered upstream — matching the reference behavior.
                    return Err(BridgeError::UpstreamError(format!(
                        "Upstream server error after model fallback (status {status})"
                    )));
                }

                if status == StatusCode::BAD_REQUEST {
                    let body = response.bytes().await.unwrap_or_default();
                    let body_text = String::from_utf8_lossy(&body);

                    if is_rate_limit_body(&body_text) {
                        warn!(
                            body = %body_text.chars().take(200).collect::<String>(),
                            "upstream encoded a rate limit as HTTP 400"
                        );
                        if advance_model(
                            state,
                            &models,
                            &mut model_index,
                            &current_model,
                            "400 rate limit",
                        ) {
                            retry_count = 0;
                            last_failed_proxy = None;
                            continue;
                        }

                        if retry_count < max_retries {
                            state.metrics.record_retry(RetryMetricClass::RateLimit);
                            retry_count += 1;
                            apply_rate_limit_penalty(state, &route, retry_count, None).await;
                            last_failed_proxy = None;
                            sleep_backoff(state, retry_count, route.proxy_index).await;
                            continue;
                        }

                        return Err(BridgeError::UpstreamError(format!(
                            "Rate limited through HTTP 400 after {retry_count} retries"
                        )));
                    }

                    let mut compatibility_changed = false;
                    if is_reasoning_content_compatibility_error(&body_text)
                        && compat_sanitize_rounds < MAX_COMPAT_SANITIZE_ROUNDS
                    {
                        compatibility_changed |= compatible_request.repair_missing_tool_reasoning();
                        if !compatibility_changed {
                            compatibility_changed |=
                                compatible_request.disable_reasoning_compatibility();
                        }
                    }
                    if is_grammar_constraint_compatibility_error(&body_text)
                        && compat_sanitize_rounds < MAX_COMPAT_SANITIZE_ROUNDS
                    {
                        compatibility_changed |= compatible_request.strip_response_format();
                    }
                    if compatibility_changed {
                        compat_sanitize_rounds += 1;
                        state.metrics.record_retry(RetryMetricClass::ProviderClient);
                        warn!(
                            body = %body_text.chars().take(240).collect::<String>(),
                            round = compat_sanitize_rounds,
                            "sanitized provider-incompatible request fields after HTTP 400"
                        );
                        continue;
                    }

                    if advance_model(
                        state,
                        &models,
                        &mut model_index,
                        &current_model,
                        "400 provider error",
                    ) {
                        retry_count = 0;
                        last_failed_proxy = None;
                        continue;
                    }

                    // Non-rate-limit 400s are deterministic request errors —
                    // retrying only multiplies latency. Surface the API error
                    // immediately (see ProviderServer branch).
                    return Err(BridgeError::UpstreamError(
                        "Upstream returned HTTP 400 after model fallback".to_string(),
                    ));
                }

                // HTTP 402 Payment Required: billing or credit issue on the
                // upstream provider. This is never retryable — surface it
                // immediately with a clear error instead of masking it as 502.
                if status == StatusCode::PAYMENT_REQUIRED {
                    let body = response.bytes().await.unwrap_or_default();
                    let body_text = String::from_utf8_lossy(&body);
                    warn!(
                        body = %body_text.chars().take(300).collect::<String>(),
                        "upstream returned 402 Payment Required — likely exhausted credits or missing billing"
                    );
                    return Err(BridgeError::PaymentRequired(
                        "Upstream API requires payment. Check your API credits/billing on the provider dashboard.".to_string(),
                    ));
                }

                return Ok(LeasedResponse::new(response, route.lease.take()));
            }
            Err(error) => {
                let failure_class = classify_reqwest_error(&error);
                if retry_count < max_retries {
                    state
                        .metrics
                        .record_retry(retry_metric_class(failure_class));
                    retry_count += 1;
                    if let Some(index) = route.proxy_index {
                        warn!(
                            proxy_index = index,
                            proxy_url = ?route.proxy_url,
                            %error,
                            ?failure_class,
                            retry_count,
                            max_retries,
                            "network error through proxy"
                        );
                        state.proxy_pool.write().await.record_failure(index);
                        last_failed_proxy = Some(index);
                    } else {
                        warn!(
                            %error,
                            ?failure_class,
                            retry_count,
                            max_retries,
                            "direct upstream network error; host WARP mutation is forbidden"
                        );
                    }
                    sleep_backoff(state, retry_count, route.proxy_index).await;
                    continue;
                }

                if advance_model(
                    state,
                    &models,
                    &mut model_index,
                    &current_model,
                    "network error",
                ) {
                    retry_count = 0;
                    last_failed_proxy = None;
                    continue;
                }

                return Err(BridgeError::UpstreamError(format!(
                    "Network error after {retry_count} retries: {error}"
                )));
            }
        }
    }
}

fn is_reasoning_content_compatibility_error(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("reasoning_content")
        && (lower.contains("invalid_request")
            || lower.contains("unsupported")
            || lower.contains("must"))
}

fn is_grammar_constraint_compatibility_error(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("grammar-constrained")
        || lower.contains("grammar constrained")
        || (lower.contains("response_format") && lower.contains("unsupported"))
}

async fn select_route(
    state: &AppState,
    routing_key: &str,
    excluded_proxy: Option<usize>,
) -> Result<SelectedRoute, BridgeError> {
    if state.config.egress.mode == crate::config::EgressMode::Direct {
        return Ok(SelectedRoute {
            client: state.http_client.clone(),
            proxy_url: None,
            proxy_index: None,
            lease: None,
        });
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let (selection, recovery_in_progress, route_availability_pending, retry_after) = {
            let mut pool = state.proxy_pool.write().await;
            if pool.proxies.is_empty() {
                return Err(BridgeError::UpstreamError(
                    "Proxy egress mode is configured but the proxy pool is empty".to_string(),
                ));
            }

            let selection = match excluded_proxy {
                Some(index) => pool.get_client_excluding(routing_key, index),
                None => pool.get_client(routing_key),
            }
            .and_then(|(client, proxy_url, proxy_index)| {
                pool.begin_lease(proxy_index)
                    .map(|lease| (client, proxy_url, proxy_index, lease))
            });
            let recovery_in_progress = pool.recovery_in_progress();
            let route_availability_pending = pool.route_availability_pending();
            let retry_after = pool.minimum_rate_limit_remaining();
            (
                selection,
                recovery_in_progress,
                route_availability_pending,
                retry_after,
            )
        };

        if let Some((client, proxy_url, proxy_index, lease)) = selection {
            return Ok(SelectedRoute {
                client,
                proxy_url: Some(proxy_url),
                proxy_index: Some(proxy_index),
                lease: Some(lease),
            });
        }

        if (recovery_in_progress || route_availability_pending)
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(250)).await;
            continue;
        }

        if let Some(remaining) = retry_after {
            let retry_after = client_retry_after(remaining);
            return Err(BridgeError::RateLimited(format!(
                "no unique healthy proxy exit is currently available; managed recovery is still running; retry after {} second(s)",
                retry_after.as_secs()
            )));
        }

        return Err(BridgeError::UpstreamError(
            "Proxy pool is configured but no eligible egress route is available".to_string(),
        ));
    }
}

async fn record_transport_success(state: &AppState, proxy_index: Option<usize>) {
    if let Some(index) = proxy_index {
        state.proxy_pool.write().await.record_success(index);
    }
}

async fn apply_rate_limit_penalty(
    state: &AppState,
    route: &SelectedRoute,
    retry_count: u32,
    retry_after: Option<Duration>,
) {
    if let Some(index) = route.proxy_index {
        let mut pool = state.proxy_pool.write().await;
        if let Some(duration) = retry_after {
            pool.mark_rate_limited(index, duration);
            info!(
                proxy_index = index,
                cooldown_secs = duration.as_secs(),
                "using upstream Retry-After value"
            );
        } else {
            pool.mark_rate_limited_adaptive(index, retry_count);
        }
    } else {
        warn!(
            retry_count,
            "direct upstream rate limit received; host WARP mutation is forbidden"
        );
    }
}

fn retry_metric_class(class: FailureClass) -> RetryMetricClass {
    match class {
        FailureClass::Transport => RetryMetricClass::Transport,
        FailureClass::Timeout => RetryMetricClass::Timeout,
        FailureClass::RateLimit => RetryMetricClass::RateLimit,
        FailureClass::ProviderClient => RetryMetricClass::ProviderClient,
        FailureClass::ProviderServer => RetryMetricClass::ProviderServer,
        FailureClass::MalformedResponse | FailureClass::Cancelled => {
            RetryMetricClass::MalformedResponse
        }
    }
}

fn advance_model(
    state: &AppState,
    models: &[String],
    model_index: &mut usize,
    current_model: &str,
    reason: &str,
) -> bool {
    if *model_index + 1 >= models.len() {
        return false;
    }

    *model_index += 1;
    state.metrics.record_model_fallback();
    warn!(
        %reason,
        from_model = %current_model,
        to_model = %models[*model_index],
        "switching to configured fallback model"
    );
    true
}

async fn sleep_backoff(state: &AppState, retry_count: u32, proxy_index: Option<usize>) {
    let seed = proxy_index.unwrap_or_default() as u64 ^ u64::from(retry_count);
    let delay = bounded_backoff(&state.config.retry, retry_count, seed);
    info!(?delay, retry_count, "waiting before upstream retry");
    tokio::time::sleep(delay).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BridgeConfig;
    use crate::infrastructure::warp::{WarpController, WarpError, WarpStatus};
    use crate::proxy_pool::{CircuitState, HealthState};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[derive(Debug, Default)]
    struct CountingWarpController {
        reconnects: AtomicUsize,
    }

    #[async_trait]
    impl WarpController for CountingWarpController {
        async fn connect(&self) -> Result<(), WarpError> {
            Ok(())
        }

        async fn disconnect(&self) -> Result<(), WarpError> {
            Ok(())
        }

        async fn status(&self) -> Result<WarpStatus, WarpError> {
            Ok(WarpStatus::Connected)
        }

        async fn reconnect(&self) -> Result<(), WarpError> {
            self.reconnects.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn detects_provider_compatibility_errors() {
        assert!(is_reasoning_content_compatibility_error(
            r#"{"error":{"type":"invalid_request_error","message":"The `reasoning_content` is invalid"}}"#
        ));
        assert!(is_grammar_constraint_compatibility_error(
            "DFLASH speculative decoding does not support grammar-constrained decoding"
        ));
        assert!(!is_grammar_constraint_compatibility_error(
            "ordinary bad request"
        ));
    }

    #[test]
    fn sanitizes_mapped_request_for_provider_retry() {
        let mut request = OpenAiRequest {
            model: "deepseek-v4-flash-free".to_string(),
            messages: vec![crate::opencode::types::OpenAiMessage {
                role: "assistant".to_string(),
                content: Some("visible".to_string()),
                reasoning_content: None,
                tool_calls: Some(vec![crate::opencode::types::OpenAiToolCall {
                    id: "call_1".to_string(),
                    tool_type: "function".to_string(),
                    function: crate::opencode::types::OpenAiFunctionCall {
                        name: "Read".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                tool_call_id: None,
                name: None,
            }],
            thinking: Some(crate::opencode::types::OpenAiThinkingConfig {
                thinking_type: "enabled".to_string(),
            }),
            reasoning_effort: Some("max".to_string()),
            response_format: Some(serde_json::json!({"type":"json_object"})),
            ..OpenAiRequest::default()
        };
        assert!(request.repair_missing_tool_reasoning());
        assert_eq!(
            request.messages[0].reasoning_content.as_deref(),
            Some("Tool call continuation.")
        );
        assert!(!request.repair_missing_tool_reasoning());
        assert!(request.disable_reasoning_compatibility());
        assert!(request.messages[0].reasoning_content.is_none());
        assert_eq!(
            request
                .thinking
                .as_ref()
                .map(|value| value.thinking_type.as_str()),
            Some("disabled")
        );
        assert!(request.reasoning_effort.is_none());
        assert!(request.strip_response_format());
        assert!(request.response_format.is_none());
        assert!(!request.strip_response_format());
    }

    #[test]
    fn sanitizes_openai_passthrough_request_for_provider_retry() {
        let mut request = OpenAiInboundRequest {
            model: "deepseek-v4-flash-free".to_string(),
            messages: vec![serde_json::json!({
                "role":"assistant",
                "content":"visible",
                "tool_calls":[{"id":"call_1","type":"function","function":{"name":"Read","arguments":"{}"}}]
            })],
            stream: false,
            extra: std::collections::BTreeMap::from([
                (
                    "thinking".to_string(),
                    serde_json::json!({"type":"enabled"}),
                ),
                ("reasoning_effort".to_string(), serde_json::json!("max")),
                (
                    "response_format".to_string(),
                    serde_json::json!({"type":"json_object"}),
                ),
            ]),
        };
        assert!(request.repair_missing_tool_reasoning());
        assert_eq!(
            request.messages[0]
                .get("reasoning_content")
                .and_then(serde_json::Value::as_str),
            Some("Tool call continuation.")
        );
        assert!(request.disable_reasoning_compatibility());
        assert!(request.messages[0].get("reasoning_content").is_none());
        assert_eq!(
            request.extra.get("thinking"),
            Some(&serde_json::json!({"type":"disabled"}))
        );
        assert!(!request.extra.contains_key("reasoning_effort"));
        assert!(request.strip_response_format());
        assert!(!request.extra.contains_key("response_format"));
    }

    #[tokio::test]
    async fn direct_rate_limit_never_reconnects_host_warp() {
        let mut config = BridgeConfig::default();
        config.egress.mode = crate::config::EgressMode::Direct;
        let warp = Arc::new(CountingWarpController::default());
        let state = AppState::new_with_adapters(
            config.clone(),
            Arc::new(crate::docker::DockerCliRuntime::from_config(&config)),
            warp.clone(),
        );
        let route = SelectedRoute {
            client: state.http_client.clone(),
            proxy_url: None,
            proxy_index: None,
            lease: None,
        };

        apply_rate_limit_penalty(&state, &route, 1, Some(Duration::from_secs(60))).await;

        assert_eq!(warp.reconnects.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn direct_mode_ignores_configured_proxy_pool() {
        let mut config = BridgeConfig {
            primary_proxies: Some(vec!["socks5://127.0.0.1:40001".to_string()]),
            ..BridgeConfig::default()
        };
        config.egress.mode = crate::config::EgressMode::Direct;
        let state = AppState::new(config);

        let route = select_route(&state, "test", None)
            .await
            .expect("direct mode should bypass configured proxies");
        assert!(route.proxy_url.is_none());
        assert!(route.proxy_index.is_none());
        assert!(route.lease.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limited_route_reports_recovery_cadence_not_quota_deadline() {
        let mut config = BridgeConfig {
            primary_proxies: Some(vec!["socks5://127.0.0.1:40001".to_string()]),
            ..BridgeConfig::default()
        };
        config.egress.mode = crate::config::EgressMode::Proxy;
        let state = AppState::new(config);
        {
            let mut pool = state.proxy_pool.write().await;
            pool.mark_rate_limited(0, Duration::from_secs(47_897));
            assert!(
                pool.minimum_rate_limit_remaining()
                    .is_some_and(|remaining| remaining.as_secs() > 47_000),
                "provider quota deadline must remain quarantined internally"
            );
        }

        let error = select_route(&state, "test", None)
            .await
            .err()
            .expect("rate-limited proxy pool should fail closed");
        let message = error.to_string();
        assert!(
            message.contains("managed recovery is still running"),
            "{message}"
        );
        assert!(message.contains("retry after 30 second(s)"), "{message}");
        assert!(!message.contains("47897"), "{message}");
        assert!(
            state
                .proxy_pool
                .read()
                .await
                .minimum_rate_limit_remaining()
                .is_some_and(|remaining| remaining.as_secs() > 47_000),
            "client-facing cap must not shorten internal quota quarantine"
        );
    }

    #[tokio::test]
    async fn configured_proxy_pool_never_silently_falls_back_to_direct() {
        let mut config = BridgeConfig {
            primary_proxies: Some(vec!["socks5://127.0.0.1:40001".to_string()]),
            ..BridgeConfig::default()
        };
        config.egress.mode = crate::config::EgressMode::Proxy;
        let state = AppState::new(config);
        {
            let mut pool = state.proxy_pool.write().await;
            pool.proxies[0].health = HealthState::Unhealthy;
            pool.proxies[0].circuit = CircuitState::Open {
                until: std::time::Instant::now() + std::time::Duration::from_secs(60),
            };
        }

        let error = select_route(&state, "test", None)
            .await
            .err()
            .expect("unavailable proxy pool should fail closed");
        assert!(error
            .to_string()
            .contains("no eligible egress route is available"));
    }
}
