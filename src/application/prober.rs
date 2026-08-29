//! Upstream free-model prober and auto-detection.

use crate::application::models::{self, FreeModel, ModelProfile, FREE_MODELS};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ModelStatus {
    Online,
    RateLimited,
    Unavailable,
    Unknown,
}

impl std::fmt::Display for ModelStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Online => write!(f, "ONLINE"),
            Self::RateLimited => write!(f, "RATE_LIMITED"),
            Self::Unavailable => write!(f, "UNAVAILABLE"),
            Self::Unknown => write!(f, "UNKNOWN"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbedModel {
    pub id: String,
    pub label: String,
    pub provider: String,
    pub context_window: usize,
    pub auto_compact_window: usize,
    pub max_output_tokens: usize,
    pub supports_thinking: bool,
    pub status: ModelStatus,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpstreamModelItem {
    id: String,
}

#[derive(Debug, Deserialize)]
struct UpstreamModelsResponse {
    #[serde(default)]
    data: Vec<UpstreamModelItem>,
}

/// Filter whether a model identifier represents a free OpenCode tier model.
pub fn is_free_model_id(id: &str) -> bool {
    let clean = id.strip_prefix("opencode/").unwrap_or(id);
    clean.ends_with("-free")
        || clean == "big-pickle"
        || models::is_supported_free_model(id)
        || models::is_supported_free_model(clean)
}

/// Probe the status of a specific model on the upstream server.
pub async fn probe_single_model(
    client: &Client,
    base_url: &str,
    model_id: &str,
    timeout: Duration,
) -> (ModelStatus, Option<u64>, Option<String>) {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let clean_id = model_id.strip_prefix("opencode/").unwrap_or(model_id);

    let start = Instant::now();
    let body = serde_json::json!({
        "model": clean_id,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 1
    });

    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .timeout(timeout)
        .json(&body)
        .send()
        .await;

    let latency = start.elapsed().as_millis() as u64;

    match resp {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                (ModelStatus::Online, Some(latency), None)
            } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                (
                    ModelStatus::RateLimited,
                    Some(latency),
                    Some("Rate limit exceeded (429)".to_string()),
                )
            } else {
                let err_body = resp.text().await.unwrap_or_default();
                let lower = err_body.to_ascii_lowercase();
                if lower.contains("unavailable")
                    || lower.contains("not supported")
                    || lower.contains("not found")
                    || lower.contains("expired")
                {
                    (
                        ModelStatus::Unavailable,
                        Some(latency),
                        Some(clean_error_message(&err_body)),
                    )
                } else if lower.contains("rate limit") || lower.contains("quota") {
                    (
                        ModelStatus::RateLimited,
                        Some(latency),
                        Some(clean_error_message(&err_body)),
                    )
                } else {
                    (
                        ModelStatus::Unavailable,
                        Some(latency),
                        Some(clean_error_message(&err_body)),
                    )
                }
            }
        }
        Err(err) => (
            ModelStatus::Unavailable,
            None,
            Some(format!("Network/timeout: {err}")),
        ),
    }
}

fn clean_error_message(raw: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(raw) {
        if let Some(msg) = val
            .pointer("/error/message")
            .or_else(|| val.pointer("/message"))
            .and_then(|v| v.as_str())
        {
            return msg.to_string();
        }
    }
    raw.chars().take(120).collect()
}

/// Fetch list of all free models from upstream, optionally probing their live availability.
pub async fn fetch_and_probe_free_models(
    client: &Client,
    base_url: &str,
    probe_live: bool,
) -> Vec<ProbedModel> {
    let mut catalog_models: Vec<FreeModel> = FREE_MODELS.to_vec();

    let models_url = format!("{}/models", base_url.trim_end_matches('/'));
    if let Ok(resp) = client
        .get(&models_url)
        .timeout(Duration::from_secs(4))
        .send()
        .await
    {
        if let Ok(parsed) = resp.json::<UpstreamModelsResponse>().await {
            for item in parsed.data {
                if is_free_model_id(&item.id)
                    && !catalog_models.iter().any(|m| {
                        m.id == item.id
                            || m.id.strip_prefix("opencode/").unwrap_or(m.id)
                                == item.id.strip_prefix("opencode/").unwrap_or(&item.id)
                    })
                {
                    catalog_models.push(FreeModel {
                        id: Box::leak(item.id.into_boxed_str()),
                        label: "OpenCode Free Model",
                        provider: "OpenCode Zen",
                        protocol: "openai_chat_completions",
                        limited_time: true,
                        privacy_notice: "Free-period model.",
                        context_window: 128_000,
                        max_output_tokens: 16_384,
                        supports_thinking: false,
                    });
                }
            }
        }
    }

    if !probe_live {
        return catalog_models
            .into_iter()
            .map(|m| ProbedModel {
                id: m.id.to_string(),
                label: m.label.to_string(),
                provider: m.provider.to_string(),
                context_window: m.context_window,
                auto_compact_window: m.auto_compact_window(),
                max_output_tokens: m.max_output_tokens,
                supports_thinking: m.supports_thinking,
                status: ModelStatus::Unknown,
                latency_ms: None,
                error: None,
            })
            .collect();
    }

    let mut tasks = Vec::new();
    for m in catalog_models {
        let client_clone = client.clone();
        let base_url_str = base_url.to_string();
        tasks.push(tokio::spawn(async move {
            let (status, latency, error) =
                probe_single_model(&client_clone, &base_url_str, m.id, Duration::from_secs(5))
                    .await;
            ProbedModel {
                id: m.id.to_string(),
                label: m.label.to_string(),
                provider: m.provider.to_string(),
                context_window: m.context_window,
                auto_compact_window: m.auto_compact_window(),
                max_output_tokens: m.max_output_tokens,
                supports_thinking: m.supports_thinking,
                status,
                latency_ms: latency,
                error,
            }
        }));
    }

    let mut results = Vec::new();
    for task in tasks {
        if let Ok(res) = task.await {
            results.push(res);
        }
    }

    results
}

/// Detect the best currently responsive free model.
pub async fn detect_best_free_model(client: &Client, base_url: &str) -> Option<ModelProfile> {
    let probed = fetch_and_probe_free_models(client, base_url, true).await;
    let online_model = probed
        .into_iter()
        .find(|m| m.status == ModelStatus::Online)
        .map(|m| models::resolve_model_profile(&m.id));

    online_model.or_else(|| {
        FREE_MODELS
            .iter()
            .find(|m| m.id != "opencode/x-preview-f-free")
            .map(|m| m.to_profile())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_free_model_id() {
        assert!(is_free_model_id("mimo-v2.5-free"));
        assert!(is_free_model_id("opencode/nemotron-3-ultra-free"));
        assert!(is_free_model_id("big-pickle"));
        assert!(!is_free_model_id("gpt-4o"));
        assert!(!is_free_model_id("claude-sonnet-4-5"));
    }

    #[test]
    fn test_clean_error_message() {
        let json_err = r#"{"error":{"message":"Model is unavailable."}}"#;
        assert_eq!(clean_error_message(json_err), "Model is unavailable.");
    }
}
