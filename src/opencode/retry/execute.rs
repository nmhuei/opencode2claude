//! Upstream request execution with model fallback and egress-aware retries.

use super::policy::{
    bounded_backoff, build_model_retry_list, classify_reqwest_error, classify_status,
    is_rate_limit_body, is_reasoning_heavy_model, parse_retry_after, FailureClass,
};
use super::response::LeasedResponse;
use super::warp::reconnect_warp;
use crate::error::BridgeError;
use crate::observability::RetryMetricClass;
use crate::opencode::types::OpenAiRequest;
use crate::proxy_pool::EgressLease;
use crate::state::AppState;
use reqwest::{Client, StatusCode};
use std::time::Duration;
use tracing::{info, warn};

struct SelectedRoute {
    client: Client,
    proxy_url: Option<String>,
    proxy_index: Option<usize>,
    lease: Option<EgressLease>,
}

pub(crate) async fn execute_with_warp_retry(
    state: &AppState,
    routing_key: &str,
    request: &OpenAiRequest,
) -> Result<LeasedResponse, BridgeError> {
    let max_retries = state.config.retry.max_network_attempts as u32;
    let max_provider_retries = state.config.retry.max_provider_attempts;
    let models = build_model_retry_list(request, &state.config.retry);
    let upstream_url = format!(
        "{}/chat/completions",
        state.config.retry.upstream_base_url.trim_end_matches('/')
    );

    if request.stream && is_reasoning_heavy_model(&request.model) && models.len() == 1 {
        info!(
            model = %request.model,
            "preserving reasoning-stream semantics without implicit fallback"
        );
    }

    let mut model_index = 0usize;
    let mut retry_count = 0u32;
    let mut last_failed_proxy = None;

    loop {
        let current_model = models.get(model_index).unwrap_or(&request.model).clone();
        let mut attempt_request = request.clone();
        attempt_request.model = current_model.clone();

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
                        last_failed_proxy = route.proxy_index;
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

                    if retry_count < max_retries {
                        state.metrics.record_retry(RetryMetricClass::ProviderServer);
                        retry_count += 1;
                        warn!(
                            %status,
                            retry_count,
                            max_retries,
                            "upstream server error; retrying without penalizing egress"
                        );
                        sleep_backoff(state, retry_count, route.proxy_index).await;
                        continue;
                    }

                    return Err(BridgeError::UpstreamError(format!(
                        "Upstream server error after {retry_count} retries (status {status})"
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
                            last_failed_proxy = route.proxy_index;
                            sleep_backoff(state, retry_count, route.proxy_index).await;
                            continue;
                        }

                        return Err(BridgeError::UpstreamError(format!(
                            "Rate limited through HTTP 400 after {retry_count} retries"
                        )));
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

                    if retry_count < max_provider_retries {
                        state.metrics.record_retry(RetryMetricClass::ProviderClient);
                        retry_count += 1;
                        warn!(
                            retry_count,
                            max_retries = max_provider_retries,
                            body = %body_text.chars().take(200).collect::<String>(),
                            "upstream returned a non-rate-limit 400; retrying without penalizing egress"
                        );
                        sleep_backoff(state, retry_count, route.proxy_index).await;
                        continue;
                    }

                    return Err(BridgeError::UpstreamError(format!(
                        "Upstream returned HTTP 400 after {max_provider_retries} provider retry attempt(s)"
                    )));
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
                            "direct upstream network error; reconnecting host WARP"
                        );
                        reconnect_warp(state.warp_controller.as_ref()).await;
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

async fn select_route(
    state: &AppState,
    routing_key: &str,
    excluded_proxy: Option<usize>,
) -> Result<SelectedRoute, BridgeError> {
    let mut pool = state.proxy_pool.write().await;
    if pool.proxies.is_empty() {
        if state.config.egress.mode == crate::config::EgressMode::Direct {
            return Ok(SelectedRoute {
                client: state.http_client.clone(),
                proxy_url: None,
                proxy_index: None,
                lease: None,
            });
        }
        return Err(BridgeError::UpstreamError(
            "Proxy egress mode is configured but the proxy pool is empty".to_string(),
        ));
    }

    let selection = match excluded_proxy {
        Some(index) => pool.get_client_excluding(routing_key, index),
        None => pool.get_client(routing_key),
    };

    let (client, proxy_url, proxy_index) = selection.ok_or_else(|| {
        BridgeError::UpstreamError(
            "Proxy pool is configured but no eligible egress route is available".to_string(),
        )
    })?;
    let lease = pool.begin_lease(proxy_index).ok_or_else(|| {
        BridgeError::UpstreamError("failed to acquire egress request lease".to_string())
    })?;
    Ok(SelectedRoute {
        client,
        proxy_url: Some(proxy_url),
        proxy_index: Some(proxy_index),
        lease: Some(lease),
    })
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
        reconnect_warp(state.warp_controller.as_ref()).await;
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
    use crate::proxy_pool::{CircuitState, HealthState};

    #[tokio::test]
    async fn configured_proxy_pool_never_silently_falls_back_to_direct() {
        let config = BridgeConfig {
            primary_proxies: Some(vec!["socks5://127.0.0.1:40001".to_string()]),
            ..BridgeConfig::default()
        };
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
