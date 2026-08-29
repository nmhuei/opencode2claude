//! Retry and model-fallback policy.

use crate::config::RetryConfig;
use std::collections::HashSet;
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureClass {
    Transport,
    Timeout,
    RateLimit,
    ProviderClient,
    ProviderServer,
    MalformedResponse,
    Cancelled,
}

pub(super) fn is_rate_limit_body(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    [
        "rate limit",
        "rate_limit",
        "quota exceeded",
        "quota_exceeded",
        "too many requests",
        "throttl",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

pub(super) fn is_reasoning_heavy_model(model: &str) -> bool {
    let name = model.to_ascii_lowercase();
    (name.contains("deepseek") && (name.contains("r1") || name.contains("reasoner")))
        || name.contains("reasoning")
        || name.contains("-r1")
}

pub(super) fn build_model_retry_list(
    model: &str,
    stream: bool,
    retry: &RetryConfig,
) -> Vec<String> {
    let configured = retry
        .model_fallbacks
        .iter()
        .map(|model| crate::opencode::mapper::map_model_name(model.trim()))
        .filter(|model| !model.is_empty())
        .collect::<Vec<_>>();

    let mut models = vec![model.to_string()];
    if configured.is_empty() {
        if retry.default_fallbacks_enabled
            && !(stream && is_reasoning_heavy_model(model))
            && (model.contains("deepseek-v4-flash-free") || model.contains("nemotron-3-ultra-free"))
        {
            models.extend([
                "deepseek-v4-flash-free".to_string(),
                "nemotron-3-ultra-free".to_string(),
            ]);
        }
    } else {
        models.extend(configured);
    }

    let mut seen = HashSet::new();
    models.retain(|model| seen.insert(model.clone()));
    models
}

pub(super) fn classify_status(status: reqwest::StatusCode, body: Option<&str>) -> FailureClass {
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || body.is_some_and(is_rate_limit_body) {
        FailureClass::RateLimit
    } else if status.is_server_error() {
        FailureClass::ProviderServer
    } else if status.is_client_error() {
        FailureClass::ProviderClient
    } else {
        FailureClass::MalformedResponse
    }
}

/// Parse Retry-After as either delta seconds or an HTTP-date.
pub(super) fn classify_reqwest_error(error: &reqwest::Error) -> FailureClass {
    if error.is_timeout() {
        FailureClass::Timeout
    } else {
        FailureClass::Transport
    }
}

pub(crate) fn cancellation_failure() -> FailureClass {
    FailureClass::Cancelled
}

pub(super) fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    // RFC 7231 IMF-fixdate. Avoid a heavyweight date dependency by using the
    // `httpdate` crate, shared with HTTP semantics elsewhere.
    let when = httpdate::parse_http_date(value).ok()?;
    when.duration_since(now).ok()
}

/// Keep provider quota deadlines internally, but never tell a foreground
/// client to sleep for hours while managed proxy recovery keeps rotating exits.
/// The recovery worker retries exhausted rotations every 30 seconds, so this
/// is the longest useful client-visible delay.
pub(super) fn client_retry_after(remaining: Duration) -> Duration {
    remaining
        .max(Duration::from_secs(1))
        .min(Duration::from_secs(30))
}

/// Longest provider `Retry-After` value honored for internal quarantine
/// bookkeeping. Real quota windows are hours at most; the clamp exists because
/// the raw value feeds `Instant + duration`, which panics on overflow (an
/// arbitrary upstream header must never be able to panic the bridge). The cap
/// stays far above any legitimate window and above the client-visible 30s
/// recovery cadence.
pub(super) const MAX_RESPECTED_RETRY_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);

pub(super) fn clamp_provider_retry_after(delay: Duration) -> Duration {
    delay.min(MAX_RESPECTED_RETRY_AFTER)
}

pub(super) fn bounded_backoff(retry: &RetryConfig, attempt: u32, jitter_seed: u64) -> Duration {
    let factor = 1_u32.checked_shl(attempt.min(16)).unwrap_or(u32::MAX);
    let base_ms = retry
        .base_backoff
        .as_millis()
        .saturating_mul(u128::from(factor));
    // Deterministic 75%-125% jitter keeps tests reproducible and avoids
    // synchronized retries. Jitter is applied before the cap so
    // `max_backoff` is enforced exactly instead of up to +25%.
    let jitter_percent = 75_u128 + u128::from(jitter_seed % 51);
    let jittered_ms = base_ms.saturating_mul(jitter_percent) / 100;
    let capped_ms = jittered_ms.min(retry.max_backoff.as_millis());
    Duration::from_millis(capped_ms.min(u128::from(u64::MAX)) as u64)
}
