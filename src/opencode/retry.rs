//! Retry logic and WARP IP rotation for upstream API requests.
//!
//! Provides exponential-backoff retry with proxy cooldown management
//! and WARP IP rotation fallback for rate-limit resilience.
//!
//! Extracted from `forward.rs` during module split.

use crate::error::BridgeError;
use crate::opencode::types::OpenAiRequest;
use crate::state::AppState;
use std::time::Duration;
use tracing::{info, warn};

/// Rotate WARP IP address by disconnecting and reconnecting.
async fn rotate_warp_ip() {
    info!("Rotating WARP IP address...");

    let disconnect = tokio::process::Command::new("warp-cli")
        .arg("disconnect")
        .output()
        .await;

    match disconnect {
        Ok(output) if output.status.success() => {
            info!("warp-cli disconnect succeeded");
        }
        Ok(output) => {
            warn!(
                "warp-cli disconnect returned non-zero: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(e) => {
            warn!("warp-cli disconnect failed (maybe not installed?): {}", e);
            return;
        }
    }

    tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

    let connect = tokio::process::Command::new("warp-cli")
        .arg("connect")
        .output()
        .await;

    match connect {
        Ok(output) if output.status.success() => {
            info!("warp-cli connect succeeded");
        }
        Ok(output) => {
            warn!(
                "warp-cli connect returned non-zero: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(2500)).await;
            return;
        }
        Err(e) => {
            warn!("warp-cli connect failed: {}", e);
            return;
        }
    }

    tokio::time::sleep(tokio::time::Duration::from_millis(2500)).await;
    info!("WARP IP address rotated successfully.");
}

/// Check if a response body text indicates a rate-limit error.
fn is_rate_limit_body(body: &str) -> bool {
    let lower = body.to_lowercase();
    lower.contains("rate")
        || lower.contains("limit")
        || lower.contains("quota")
        || lower.contains("too many")
        || lower.contains("throttl")
}

/// Maximum retries for 400 provider errors (distinct from rate-limit retries).
const MAX_PROVIDER_RETRIES: u32 = 1;

fn is_reasoning_heavy_model(model: &str) -> bool {
    let name = model.to_ascii_lowercase();
    (name.contains("deepseek") && (name.contains("r1") || name.contains("reasoner")))
        || name.contains("reasoning")
        || name.contains("-r1")
}

fn default_fallbacks_enabled() -> bool {
    std::env::var("OPENCODE_ENABLE_DEFAULT_FALLBACKS")
        .ok()
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

fn build_model_retry_list(req_body: &OpenAiRequest) -> Vec<String> {
    let fallbacks = std::env::var("OPENCODE_MODEL_FALLBACKS")
        .ok()
        .map(|s| {
            s.split(',')
                .map(|m| crate::opencode::mapper::map_model_name(m.trim()))
                .filter(|m| !m.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut model_list = vec![req_body.model.clone()];
    if !fallbacks.is_empty() {
        model_list.extend(fallbacks);
    } else if default_fallbacks_enabled()
        && !(req_body.stream && is_reasoning_heavy_model(&req_body.model))
    {
        let m = req_body.model.clone();
        if m.contains("deepseek-v4-flash-free") || m.contains("nemotron-3-ultra-free") {
            model_list.push("deepseek-v4-flash-free".to_string());
            model_list.push("nemotron-3-ultra-free".to_string());
        }
    }

    let mut seen = std::collections::HashSet::new();
    model_list.retain(|x| seen.insert(x.clone()));
    model_list
}

/// Execute a request with exponential-backoff retry, proxy cooldown, and WARP IP rotation.
///
/// Retry strategy:
/// - 429/5xx: rate-limit retry up to `pool_size.max(3) + 2` times
/// - 400 rate-limit body: same as 429
/// - 400 provider error: retry up to 10 times
/// - Network errors: same as 429
/// - Between retries: proxies are cooled down adaptively (2^retry min × 60s)
pub(super) async fn execute_with_warp_retry(
    state: &AppState,
    api_key: &str,
    req_body: &OpenAiRequest,
) -> Result<reqwest::Response, BridgeError> {
    let pool_size = {
        let pool = state.proxy_pool.read().await;
        pool.proxies.len()
    };
    let max_retries = pool_size.max(3) + 2;

    let model_list = build_model_retry_list(req_body);
    if req_body.stream && is_reasoning_heavy_model(&req_body.model) && model_list.len() == 1 {
        info!(
            "Streaming reasoning model {} will not use implicit non-reasoning fallback; preserving thinking_delta semantics.",
            req_body.model
        );
    }

    let mut model_index = 0;
    let mut retry_count: u32 = 0;
    let mut last_failed_idx: Option<usize> = None;

    loop {
        let current_model = if model_index < model_list.len() {
            &model_list[model_index]
        } else {
            &req_body.model
        };
        let mut req_body_clone = req_body.clone();
        req_body_clone.model = current_model.clone();

        // Select the client from the proxy pool if configured
        let (client, proxy_url, idx) = {
            let mut pool = state.proxy_pool.write().await;
            let result = if let Some(exclude) = last_failed_idx {
                pool.get_client_excluding(api_key, exclude)
                    .or_else(|| pool.get_client(api_key))
            } else {
                pool.get_client(api_key)
            };
            if let Some((c, url, idx)) = result {
                (c, Some(url), Some(idx))
            } else {
                (state.http_client.clone(), None, None)
            }
        };

        let res = client
            .post("https://opencode.ai/zen/v1/chat/completions")
            .json(&req_body_clone)
            .send()
            .await;

        match res {
            Ok(response) => {
                let status = response.status();

                if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                    // 429 and 5xx are rate-limit / server errors
                    if model_index + 1 < model_list.len() {
                        model_index += 1;
                        warn!(
                            "Upstream error (status {}) on model {}. Switching to fallback model: {}",
                            status, req_body_clone.model, model_list[model_index]
                        );
                        retry_count = 0;
                        last_failed_idx = None;
                        continue;
                    }
                    if (retry_count as usize) < max_retries {
                        retry_count += 1;
                        if let (Some(idx), Some(ref url)) = (idx, &proxy_url) {
                            warn!(
                                "Upstream error (status {}) on proxy #{} ({}). Putting proxy on cool-down (attempt {}/{})...",
                                status, idx, url, retry_count, max_retries
                            );
                            let mut pool = state.proxy_pool.write().await;
                            // Try Retry-After header first (HTTP/1.1 standard)
                            let cooldown = response
                                .headers()
                                .get("retry-after")
                                .and_then(|v| v.to_str().ok())
                                .and_then(|s| s.parse::<u64>().ok())
                                .map(Duration::from_secs);
                            if let Some(d) = cooldown {
                                pool.mark_rate_limited(idx, d);
                                info!("Using Retry-After header: {}s cooldown", d.as_secs());
                            } else {
                                pool.mark_rate_limited_adaptive(idx, retry_count);
                            }
                            last_failed_idx = Some(idx);
                        } else {
                            warn!(
                                "Upstream error (status {}). Attempting to rotate WARP IP (attempt {}/{})...",
                                status, retry_count, max_retries
                            );
                            rotate_warp_ip().await;
                        }
                        let backoff = std::time::Duration::from_secs(2u64.pow(retry_count.min(4)));
                        info!("Backing off for {:?} before retry...", backoff);
                        tokio::time::sleep(backoff).await;
                        continue;
                    }
                    return Err(BridgeError::UpstreamError(format!(
                        "Upstream error after {} retries (status {})",
                        retry_count, status
                    )));
                } else if status == reqwest::StatusCode::BAD_REQUEST {
                    // 400: Read body to distinguish genuine errors from rate limits
                    let body_bytes = response.bytes().await.unwrap_or_default();
                    let body_text = String::from_utf8_lossy(&body_bytes);
                    if is_rate_limit_body(&body_text) {
                        warn!(
                            "Upstream returned 400 with rate-limit body (truncated): {}",
                            body_text.chars().take(200).collect::<String>()
                        );
                        if model_index + 1 < model_list.len() {
                            model_index += 1;
                            warn!(
                                "Upstream rate limit on model {}. Switching to fallback model: {}",
                                req_body_clone.model, model_list[model_index]
                            );
                            retry_count = 0;
                            last_failed_idx = None;
                            continue;
                        }
                        if (retry_count as usize) < max_retries {
                            retry_count += 1;
                            if let (Some(idx), Some(ref url)) = (idx, &proxy_url) {
                                warn!(
                                    "Rate-limit on proxy #{} ({}). Cool-down (attempt {}/{})...",
                                    idx, url, retry_count, max_retries
                                );
                                let mut pool = state.proxy_pool.write().await;
                                pool.mark_rate_limited_adaptive(idx, retry_count);
                                last_failed_idx = Some(idx);
                            } else {
                                rotate_warp_ip().await;
                            }
                            let backoff =
                                std::time::Duration::from_secs(2u64.pow(retry_count.min(4)));
                            info!("Backing off for {:?} before retry...", backoff);
                            tokio::time::sleep(backoff).await;
                            continue;
                        }
                        return Err(BridgeError::UpstreamError(format!(
                            "Rate limited (400) after {} retries",
                            retry_count
                        )));
                    } else {
                        // Genuine 400 error — upstream provider failure, retry up to 10x
                        if model_index + 1 < model_list.len() {
                            model_index += 1;
                            warn!(
                                "Upstream returned 400 (provider error) on model {}. Switching to fallback model: {}",
                                req_body_clone.model, model_list[model_index]
                            );
                            retry_count = 0;
                            last_failed_idx = None;
                            continue;
                        }
                        if retry_count < MAX_PROVIDER_RETRIES {
                            retry_count += 1;
                            warn!(
                                "Upstream returned 400 (provider error, attempt {}/{}, truncated): {}",
                                retry_count, MAX_PROVIDER_RETRIES,
                                body_text.chars().take(200).collect::<String>()
                            );
                            if let (Some(idx), Some(ref _url)) = (idx, &proxy_url) {
                                let mut pool = state.proxy_pool.write().await;
                                pool.mark_rate_limited(idx, Duration::from_secs(5));
                                last_failed_idx = Some(idx);
                            } else {
                                rotate_warp_ip().await;
                            }
                            let backoff =
                                std::time::Duration::from_secs(2u64.pow(retry_count.min(4)));
                            info!("Backing off for {:?} before retry...", backoff);
                            tokio::time::sleep(backoff).await;
                            continue;
                        }
                        warn!(
                            "Upstream returned 400 (failed after {} retries, truncated): {}",
                            MAX_PROVIDER_RETRIES,
                            body_text.chars().take(300).collect::<String>()
                        );
                        return Err(BridgeError::UpstreamError(
                            "Upstream returned 400 after 10 retries".to_string(),
                        ));
                    }
                } else {
                    // Success or other status — return as-is
                    // Record success on proxy since transport worked (even for 4xx)
                    if let Some(idx) = idx {
                        let mut pool = state.proxy_pool.write().await;
                        pool.record_success(idx);
                    }
                    return Ok(response);
                }
            }
            Err(e) => {
                if (retry_count as usize) < max_retries {
                    retry_count += 1;
                    if let (Some(idx), Some(ref url)) = (idx, &proxy_url) {
                        warn!(
                            "Network error connecting via proxy #{} ({}): {}. Putting proxy on cool-down (attempt {}/{})...",
                            idx, url, e, retry_count, max_retries
                        );
                        let mut pool = state.proxy_pool.write().await;
                        // Network transport error = proxy failure
                        pool.record_failure(idx);
                        info!(
                            "Recorded transport failure for proxy #{} ({}) after {}/{} retries.",
                            idx, url, retry_count, max_retries
                        );
                        last_failed_idx = Some(idx);
                    } else {
                        warn!(
                            "Network error connecting upstream: {}. Attempting to rotate WARP IP (attempt {}/{})...",
                            e, retry_count, max_retries
                        );
                        rotate_warp_ip().await;
                    }
                    // Exponential backoff
                    let backoff = std::time::Duration::from_secs(2u64.pow(retry_count.min(4)));
                    info!("Backing off for {:?} before retry...", backoff);
                    tokio::time::sleep(backoff).await;
                    continue;
                }
                if model_index + 1 < model_list.len() {
                    model_index += 1;
                    warn!(
                        "Network error on model {}: {}. Switching to fallback model: {}",
                        req_body_clone.model, e, model_list[model_index]
                    );
                    retry_count = 0;
                    last_failed_idx = None;
                    continue;
                }
                return Err(BridgeError::UpstreamError(format!(
                    "Network error after {} retries: {}",
                    retry_count, e
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opencode::types::OpenAiRequest;
    use std::sync::Mutex;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    fn request(model: &str, stream: bool) -> OpenAiRequest {
        OpenAiRequest {
            model: model.to_string(),
            messages: vec![],
            tools: None,
            tool_choice: None,
            stream,
            temperature: None,
            max_tokens: Some(32),
            include_reasoning: None,
        }
    }

    #[test]
    fn test_streaming_reasoning_model_has_no_implicit_non_reasoning_fallback() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("OPENCODE_MODEL_FALLBACKS");
        std::env::remove_var("OPENCODE_ENABLE_DEFAULT_FALLBACKS");

        let models = build_model_retry_list(&request("deepseek-v4-flash-free", true));

        assert_eq!(models, vec!["deepseek-v4-flash-free"]);
    }

    #[test]
    fn test_explicit_fallbacks_are_respected_for_reasoning_stream() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var(
            "OPENCODE_MODEL_FALLBACKS",
            "opencode/deepseek-v4-flash-free",
        );
        std::env::remove_var("OPENCODE_ENABLE_DEFAULT_FALLBACKS");

        let models = build_model_retry_list(&request("deepseek-v4-flash-free", true));

        assert_eq!(models, vec!["deepseek-v4-flash-free"]);
        std::env::remove_var("OPENCODE_MODEL_FALLBACKS");
    }

    #[test]
    fn test_default_fallbacks_can_be_enabled_for_non_reasoning_requests() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("OPENCODE_MODEL_FALLBACKS");
        std::env::set_var("OPENCODE_ENABLE_DEFAULT_FALLBACKS", "true");

        let models = build_model_retry_list(&request("nemotron-3-ultra-free", false));

        assert!(models.contains(&"nemotron-3-ultra-free".to_string()));
        assert!(models.contains(&"deepseek-v4-flash-free".to_string()));
        std::env::remove_var("OPENCODE_ENABLE_DEFAULT_FALLBACKS");
    }
}
