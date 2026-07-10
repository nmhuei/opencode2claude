//! Retry and model-fallback policy.

use crate::opencode::types::OpenAiRequest;
use std::collections::HashSet;

pub(super) const MAX_PROVIDER_RETRIES: u32 = 1;

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

pub(super) fn build_model_retry_list(request: &OpenAiRequest) -> Vec<String> {
    let configured = std::env::var("OPENCODE_MODEL_FALLBACKS")
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(crate::opencode::mapper::map_model_name)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut models = vec![request.model.clone()];
    if configured.is_empty() {
        if default_fallbacks_enabled()
            && !(request.stream && is_reasoning_heavy_model(&request.model))
        {
            let model = request.model.as_str();
            if model.contains("deepseek-v4-flash-free") || model.contains("nemotron-3-ultra-free") {
                models.extend([
                    "deepseek-v4-flash-free".to_string(),
                    "nemotron-3-ultra-free".to_string(),
                ]);
            }
        }
    } else {
        models.extend(configured);
    }

    let mut seen = HashSet::new();
    models.retain(|model| seen.insert(model.clone()));
    models
}

fn default_fallbacks_enabled() -> bool {
    std::env::var("OPENCODE_ENABLE_DEFAULT_FALLBACKS")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}
