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
    #[serde(default)]
    owned_by: Option<String>,
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

pub fn is_opencode_upstream(base_url: &str) -> bool {
    reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host == "opencode.ai" || host.ends_with(".opencode.ai"))
}

fn static_catalog(base_url: &str) -> Vec<FreeModel> {
    if is_opencode_upstream(base_url) {
        FREE_MODELS.to_vec()
    } else {
        Vec::new()
    }
}

pub fn catalog_models_without_network(base_url: &str) -> Vec<ProbedModel> {
    static_catalog(base_url)
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
        .collect()
}

/// Check live connectivity and responsiveness of the upstream API.
pub async fn check_upstream_health(
    client: &Client,
    base_url: &str,
    api_key: Option<&str>,
) -> Result<u64, String> {
    let models_url = format!("{}/models", base_url.trim_end_matches('/'));
    let start = Instant::now();
    let mut req = client.get(&models_url).timeout(Duration::from_secs(5));
    if let Some(key) = api_key.filter(|k| !k.trim().is_empty()) {
        req = req.header("Authorization", format!("Bearer {key}"));
    }
    match req.send().await {
        Ok(resp) => {
            let latency = start.elapsed().as_millis() as u64;
            if resp.status().is_success() {
                Ok(latency)
            } else {
                let status = resp.status();
                let err_text = resp.text().await.unwrap_or_default();
                Err(format!(
                    "Status {}: {}",
                    status,
                    clean_error_message(&err_text)
                ))
            }
        }
        Err(err) => Err(format!("Connection failed: {err}")),
    }
}

/// Probe the status of a specific model on the upstream server.
pub async fn probe_single_model(
    client: &Client,
    base_url: &str,
    api_key: Option<&str>,
    model_id: &str,
    timeout: Duration,
) -> (ModelStatus, Option<u64>, Option<String>) {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let clean_id = model_id.strip_prefix("opencode/").unwrap_or(model_id);

    let start = Instant::now();
    let body = serde_json::json!({
        "model": clean_id,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 5
    });

    let mut req = client
        .post(&url)
        .header("Content-Type", "application/json")
        .timeout(timeout);

    if let Some(key) = api_key.filter(|k| !k.trim().is_empty()) {
        req = req.header("Authorization", format!("Bearer {key}"));
    }

    let resp = req.json(&body).send().await;
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
                if lower.contains("access_denied")
                    || lower.contains("deposit required")
                    || lower.contains("restricted")
                    || lower.contains("unavailable")
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
            .or_else(|| val.pointer("/error"))
            .and_then(|v| v.as_str())
        {
            return msg.to_string();
        }
    }
    raw.chars().take(120).collect()
}

/// Fetch list of models from upstream, optionally probing their live availability.
pub async fn fetch_and_probe_models(
    client: &Client,
    base_url: &str,
    api_key: Option<&str>,
    probe_live: bool,
) -> Vec<ProbedModel> {
    let is_custom_upstream = !is_opencode_upstream(base_url);
    let mut catalog_models = static_catalog(base_url);

    let models_url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut req = client.get(&models_url).timeout(Duration::from_secs(4));
    if let Some(key) = api_key.filter(|k| !k.trim().is_empty()) {
        req = req.header("Authorization", format!("Bearer {key}"));
    }

    if let Ok(resp) = req.send().await {
        if let Ok(parsed) = resp.json::<UpstreamModelsResponse>().await {
            for item in parsed.data {
                let should_include =
                    is_custom_upstream || api_key.is_some() || is_free_model_id(&item.id);

                if should_include
                    && !catalog_models.iter().any(|m| {
                        m.id == item.id
                            || m.id.strip_prefix("opencode/").unwrap_or(m.id)
                                == item.id.strip_prefix("opencode/").unwrap_or(&item.id)
                    })
                {
                    let provider = infer_upstream_service_name(base_url, item.owned_by.as_deref());
                    let profile = models::resolve_model_profile(&item.id);
                    catalog_models.push(FreeModel {
                        id: Box::leak(item.id.into_boxed_str()),
                        label: profile.label,
                        provider: Box::leak(provider.to_string().into_boxed_str()),
                        protocol: "openai_chat_completions",
                        limited_time: false,
                        privacy_notice: "",
                        context_window: profile.context_window,
                        max_output_tokens: profile.max_output_tokens,
                        supports_thinking: profile.supports_thinking,
                    });
                }
            }
        }
    }

    if catalog_models.is_empty() && !is_custom_upstream {
        catalog_models = FREE_MODELS.to_vec();
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

    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(2));
    let mut tasks = Vec::new();
    for m in catalog_models {
        let client_clone = client.clone();
        let base_url_str = base_url.to_string();
        let api_key_opt = api_key.map(|k| k.to_string());
        let sem = semaphore.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.ok();
            tokio::time::sleep(Duration::from_millis(150)).await;
            let (status, latency, error) = probe_single_model(
                &client_clone,
                &base_url_str,
                api_key_opt.as_deref(),
                m.id,
                Duration::from_secs(5),
            )
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

/// Backward-compatible alias for fetch_and_probe_models without an explicit API key.
pub async fn fetch_and_probe_free_models(
    client: &Client,
    base_url: &str,
    probe_live: bool,
) -> Vec<ProbedModel> {
    fetch_and_probe_models(client, base_url, None, probe_live).await
}

/// Detect the best currently responsive model from upstream.
pub async fn detect_best_model(
    client: &Client,
    base_url: &str,
    api_key: Option<&str>,
) -> Option<ModelProfile> {
    let probed = fetch_and_probe_models(client, base_url, api_key, true).await;
    let online_model = probed
        .into_iter()
        .find(|m| m.status == ModelStatus::Online)
        .map(|m| {
            let mut prof = models::resolve_model_profile(&m.id);
            prof.id = Box::leak(m.id.into_boxed_str());
            prof
        });

    online_model.or_else(|| {
        FREE_MODELS
            .iter()
            .find(|m| m.id != "opencode/x-preview-f-free")
            .map(|m| m.to_profile())
    })
}

/// Infer upstream service provider name (e.g. "OpenCode", "b.ai", "Groq", etc.)
pub fn infer_upstream_service_name(base_url: &str, owned_by: Option<&str>) -> &'static str {
    let parsed = reqwest::Url::parse(base_url).ok();
    let host = parsed
        .as_ref()
        .and_then(|url| url.host_str())
        .map(str::to_ascii_lowercase);
    let port = parsed.as_ref().and_then(reqwest::Url::port);

    fn host_matches(host: &str, domain: &str) -> bool {
        host == domain || host.ends_with(&format!(".{domain}"))
    }

    if host
        .as_deref()
        .is_some_and(|h| host_matches(h, "opencode.ai"))
    {
        "OpenCode"
    } else if host.as_deref().is_some_and(|h| host_matches(h, "b.ai")) {
        "b.ai"
    } else if host.as_deref().is_some_and(|h| host_matches(h, "groq.com")) {
        "Groq"
    } else if host
        .as_deref()
        .is_some_and(|h| host_matches(h, "together.xyz") || host_matches(h, "together.ai"))
    {
        "Together AI"
    } else if host
        .as_deref()
        .is_some_and(|h| host_matches(h, "deepseek.com"))
    {
        "DeepSeek"
    } else if host
        .as_deref()
        .is_some_and(|h| host_matches(h, "openai.com"))
    {
        "OpenAI"
    } else if host
        .as_deref()
        .is_some_and(|h| host_matches(h, "anthropic.com"))
    {
        "Anthropic"
    } else if host
        .as_deref()
        .is_some_and(|h| h == "localhost" || h == "127.0.0.1")
        && port == Some(11434)
    {
        "Ollama"
    } else if host
        .as_deref()
        .is_some_and(|h| h == "localhost" || h == "127.0.0.1")
        && port == Some(8000)
    {
        "vLLM"
    } else if let Some(owned) =
        owned_by.filter(|o| !o.trim().is_empty() && *o != "system" && *o != "custom")
    {
        Box::leak(owned.to_string().into_boxed_str())
    } else if let Some(host) = host {
        let host_clean = host.strip_prefix("api.").unwrap_or(&host);
        Box::leak(host_clean.to_string().into_boxed_str())
    } else {
        "Custom API"
    }
}

/// Backward-compatible detection of free model.
pub async fn detect_best_free_model(client: &Client, base_url: &str) -> Option<ModelProfile> {
    detect_best_model(client, base_url, None).await
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

        let b_ai_err = r#"{"error":{"code":"access_denied","message":"Access restricted. Deposit required to unlock premium models. (request id: 123)","type":"api_error"}}"#;
        assert_eq!(
            clean_error_message(b_ai_err),
            "Access restricted. Deposit required to unlock premium models. (request id: 123)"
        );
    }

    #[test]
    fn custom_upstreams_have_no_fake_static_online_catalog() {
        let models = catalog_models_without_network("https://custom.example/v1");
        assert!(models.is_empty());

        let opencode = catalog_models_without_network("https://opencode.ai/zen/v1");
        assert!(!opencode.is_empty());
        assert!(opencode
            .iter()
            .all(|model| model.status == ModelStatus::Unknown));
    }

    #[test]
    fn provider_detection_uses_parsed_hosts_not_substring_matches() {
        assert!(is_opencode_upstream("https://opencode.ai/zen/v1"));
        assert!(is_opencode_upstream("https://api.opencode.ai/v1"));
        assert!(!is_opencode_upstream(
            "https://opencode.ai.attacker.example/v1"
        ));
        assert_eq!(
            infer_upstream_service_name("https://groq.com.attacker.example/v1", None),
            "groq.com.attacker.example"
        );
        assert_eq!(
            infer_upstream_service_name("https://api.groq.com/openai/v1", None),
            "Groq"
        );
    }
}
