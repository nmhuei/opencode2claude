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
const MAX_PROVIDER_RETRIES: u32 = 10;

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

    let mut retry_count: u32 = 0;
    let mut last_failed_idx: Option<usize> = None;

    loop {
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
            .json(req_body)
            .send()
            .await;

        match res {
            Ok(response) => {
                let status = response.status();

                if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                    // 429 and 5xx are rate-limit / server errors
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
                return Err(BridgeError::UpstreamError(format!(
                    "Network error after {} retries: {}",
                    retry_count, e
                )));
            }
        }
    }
}
