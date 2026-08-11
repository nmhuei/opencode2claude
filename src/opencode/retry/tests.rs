use super::policy::{
    bounded_backoff, build_model_retry_list, classify_status, client_retry_after,
    is_rate_limit_body, parse_retry_after, FailureClass,
};
use crate::config::RetryConfig;
use std::time::{Duration, SystemTime};

fn retry_config() -> RetryConfig {
    crate::config::BridgeConfig::default().retry
}

#[test]
fn streaming_reasoning_model_has_no_implicit_non_reasoning_fallback() {
    let models = build_model_retry_list("deepseek-v4-flash-free", true, &retry_config());
    assert_eq!(models, vec!["deepseek-v4-flash-free"]);
}

#[test]
fn explicit_fallbacks_are_respected_for_reasoning_stream() {
    let mut retry = retry_config();
    retry.model_fallbacks = vec!["opencode/deepseek-v4-flash-free".to_string()];
    let models = build_model_retry_list("deepseek-v4-flash-free", true, &retry);
    assert_eq!(models, vec!["deepseek-v4-flash-free"]);
}

#[test]
fn default_fallbacks_can_be_enabled_for_non_reasoning_requests() {
    let mut retry = retry_config();
    retry.default_fallbacks_enabled = true;
    let models = build_model_retry_list("nemotron-3-ultra-free", false, &retry);
    assert!(models.contains(&"nemotron-3-ultra-free".to_string()));
    assert!(models.contains(&"deepseek-v4-flash-free".to_string()));
}

#[test]
fn rate_limit_classifier_does_not_match_generic_bad_request_text() {
    assert!(!is_rate_limit_body(
        "invalid parameter: max_tokens exceeds model context limit"
    ));
    assert!(!is_rate_limit_body("unsupported tool schema"));
}

#[test]
fn rate_limit_classifier_matches_known_signals() {
    assert!(is_rate_limit_body("rate limit exceeded"));
    assert!(is_rate_limit_body("quota_exceeded"));
    assert!(is_rate_limit_body("Too Many Requests"));
}

#[test]
fn status_classifier_separates_provider_and_rate_limit_failures() {
    assert_eq!(
        classify_status(reqwest::StatusCode::TOO_MANY_REQUESTS, None),
        FailureClass::RateLimit
    );
    assert_eq!(
        classify_status(reqwest::StatusCode::BAD_GATEWAY, None),
        FailureClass::ProviderServer
    );
    assert_eq!(
        classify_status(reqwest::StatusCode::BAD_REQUEST, Some("invalid schema")),
        FailureClass::ProviderClient
    );
}

#[test]
fn retry_after_supports_delta_seconds_and_http_dates() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    assert_eq!(parse_retry_after("7", now), Some(Duration::from_secs(7)));
    let date = httpdate::fmt_http_date(now + Duration::from_secs(30));
    assert_eq!(parse_retry_after(&date, now), Some(Duration::from_secs(30)));
}

#[test]
fn client_retry_after_is_short_even_when_provider_quota_reset_is_distant() {
    assert_eq!(
        client_retry_after(Duration::from_secs(47_897)),
        Duration::from_secs(30)
    );
    assert_eq!(
        client_retry_after(Duration::from_secs(7)),
        Duration::from_secs(7)
    );
    assert_eq!(client_retry_after(Duration::ZERO), Duration::from_secs(1));
}

#[test]
fn backoff_is_bounded_and_deterministic() {
    let mut retry = retry_config();
    retry.base_backoff = Duration::from_secs(1);
    retry.max_backoff = Duration::from_secs(4);
    let first = bounded_backoff(&retry, 10, 3);
    assert_eq!(first, bounded_backoff(&retry, 10, 3));
    assert!(first <= Duration::from_secs(5));
}
