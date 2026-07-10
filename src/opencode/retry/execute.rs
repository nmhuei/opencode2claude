//! Upstream request execution with model fallback and egress-aware retries.

use super::policy::{
    build_model_retry_list, is_rate_limit_body, is_reasoning_heavy_model, MAX_PROVIDER_RETRIES,
};
use super::warp::reconnect_warp;
use crate::error::BridgeError;
use crate::opencode::types::OpenAiRequest;
use crate::state::AppState;
use reqwest::{Client, Response, StatusCode};
use std::time::Duration;
use tracing::{info, warn};

const UPSTREAM_URL: &str = "https://opencode.ai/zen/v1/chat/completions";

struct SelectedRoute {
    client: Client,
    proxy_url: Option<String>,
    proxy_index: Option<usize>,
}

pub(crate) async fn execute_with_warp_retry(
    state: &AppState,
    routing_key: &str,
    request: &OpenAiRequest,
) -> Result<Response, BridgeError> {
    let proxy_count = state.proxy_pool.read().await.proxies.len();
    let max_retries = (proxy_count.max(3) + 2) as u32;
    let models = build_model_retry_list(request);

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

        let route = select_route(state, routing_key, last_failed_proxy).await?;
        let result = route
            .client
            .post(UPSTREAM_URL)
            .json(&attempt_request)
            .send()
            .await;

        match result {
            Ok(response) => {
                record_transport_success(state, route.proxy_index).await;
                let status = response.status();

                if status == StatusCode::TOO_MANY_REQUESTS {
                    if advance_model(&models, &mut model_index, &current_model, status.as_str()) {
                        retry_count = 0;
                        last_failed_proxy = None;
                        continue;
                    }

                    if retry_count < max_retries {
                        retry_count += 1;
                        let retry_after = response
                            .headers()
                            .get("retry-after")
                            .and_then(|value| value.to_str().ok())
                            .and_then(|value| value.parse::<u64>().ok())
                            .map(Duration::from_secs);
                        apply_rate_limit_penalty(state, &route, retry_count, retry_after).await;
                        last_failed_proxy = route.proxy_index;
                        sleep_backoff(retry_count).await;
                        continue;
                    }

                    return Err(BridgeError::UpstreamError(format!(
                        "Rate limited after {retry_count} retries (status {status})"
                    )));
                }

                if status.is_server_error() {
                    if advance_model(&models, &mut model_index, &current_model, status.as_str()) {
                        retry_count = 0;
                        last_failed_proxy = None;
                        continue;
                    }

                    if retry_count < max_retries {
                        retry_count += 1;
                        warn!(
                            %status,
                            retry_count,
                            max_retries,
                            "upstream server error; retrying without penalizing egress"
                        );
                        sleep_backoff(retry_count).await;
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
                            retry_count += 1;
                            apply_rate_limit_penalty(state, &route, retry_count, None).await;
                            last_failed_proxy = route.proxy_index;
                            sleep_backoff(retry_count).await;
                            continue;
                        }

                        return Err(BridgeError::UpstreamError(format!(
                            "Rate limited through HTTP 400 after {retry_count} retries"
                        )));
                    }

                    if advance_model(
                        &models,
                        &mut model_index,
                        &current_model,
                        "400 provider error",
                    ) {
                        retry_count = 0;
                        last_failed_proxy = None;
                        continue;
                    }

                    if retry_count < MAX_PROVIDER_RETRIES {
                        retry_count += 1;
                        warn!(
                            retry_count,
                            max_retries = MAX_PROVIDER_RETRIES,
                            body = %body_text.chars().take(200).collect::<String>(),
                            "upstream returned a non-rate-limit 400; retrying without penalizing egress"
                        );
                        sleep_backoff(retry_count).await;
                        continue;
                    }

                    return Err(BridgeError::UpstreamError(format!(
                        "Upstream returned HTTP 400 after {MAX_PROVIDER_RETRIES} provider retry attempt(s)"
                    )));
                }

                return Ok(response);
            }
            Err(error) => {
                if retry_count < max_retries {
                    retry_count += 1;
                    if let Some(index) = route.proxy_index {
                        warn!(
                            proxy_index = index,
                            proxy_url = ?route.proxy_url,
                            %error,
                            retry_count,
                            max_retries,
                            "network error through proxy"
                        );
                        state.proxy_pool.write().await.record_failure(index);
                        last_failed_proxy = Some(index);
                    } else {
                        warn!(
                            %error,
                            retry_count,
                            max_retries,
                            "direct upstream network error; reconnecting host WARP"
                        );
                        reconnect_warp().await;
                    }
                    sleep_backoff(retry_count).await;
                    continue;
                }

                if advance_model(&models, &mut model_index, &current_model, "network error") {
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
        return Ok(SelectedRoute {
            client: state.http_client.clone(),
            proxy_url: None,
            proxy_index: None,
        });
    }

    let selection = match excluded_proxy {
        Some(index) => pool.get_client_excluding(routing_key, index),
        None => pool.get_client(routing_key),
    };

    selection
        .map(|(client, proxy_url, proxy_index)| SelectedRoute {
            client,
            proxy_url: Some(proxy_url),
            proxy_index: Some(proxy_index),
        })
        .ok_or_else(|| {
            BridgeError::UpstreamError(
                "Proxy pool is configured but no eligible egress route is available".to_string(),
            )
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
        reconnect_warp().await;
    }
}

fn advance_model(
    models: &[String],
    model_index: &mut usize,
    current_model: &str,
    reason: &str,
) -> bool {
    if *model_index + 1 >= models.len() {
        return false;
    }

    *model_index += 1;
    warn!(
        %reason,
        from_model = %current_model,
        to_model = %models[*model_index],
        "switching to configured fallback model"
    );
    true
}

async fn sleep_backoff(retry_count: u32) {
    let delay = Duration::from_secs(2u64.pow(retry_count.min(4)));
    info!(?delay, retry_count, "waiting before upstream retry");
    tokio::time::sleep(delay).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BridgeConfig;
    use crate::proxy_pool::ProxyStatus;

    #[tokio::test]
    async fn configured_proxy_pool_never_silently_falls_back_to_direct() {
        let config = BridgeConfig {
            primary_proxies: Some(vec!["socks5://127.0.0.1:40001".to_string()]),
            ..BridgeConfig::default()
        };
        let state = AppState::new(config);
        state.proxy_pool.write().await.proxies[0].status = ProxyStatus::Dead {
            restart_attempts: 0,
        };

        let error = select_route(&state, "test", None)
            .await
            .err()
            .expect("unavailable proxy pool should fail closed");
        assert!(error
            .to_string()
            .contains("no eligible egress route is available"));
    }
}
