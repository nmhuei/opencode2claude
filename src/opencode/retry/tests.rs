use super::policy::{build_model_retry_list, is_rate_limit_body};
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
fn streaming_reasoning_model_has_no_implicit_non_reasoning_fallback() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|error| error.into_inner());
    std::env::remove_var("OPENCODE_MODEL_FALLBACKS");
    std::env::remove_var("OPENCODE_ENABLE_DEFAULT_FALLBACKS");

    let models = build_model_retry_list(&request("deepseek-v4-flash-free", true));
    assert_eq!(models, vec!["deepseek-v4-flash-free"]);
}

#[test]
fn explicit_fallbacks_are_respected_for_reasoning_stream() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|error| error.into_inner());
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
fn default_fallbacks_can_be_enabled_for_non_reasoning_requests() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|error| error.into_inner());
    std::env::remove_var("OPENCODE_MODEL_FALLBACKS");
    std::env::set_var("OPENCODE_ENABLE_DEFAULT_FALLBACKS", "true");

    let models = build_model_retry_list(&request("nemotron-3-ultra-free", false));
    assert!(models.contains(&"nemotron-3-ultra-free".to_string()));
    assert!(models.contains(&"deepseek-v4-flash-free".to_string()));
    std::env::remove_var("OPENCODE_ENABLE_DEFAULT_FALLBACKS");
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
