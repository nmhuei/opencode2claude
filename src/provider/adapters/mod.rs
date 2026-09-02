//! Protocol-specific request preparation. Request execution owns no provider
//! inference; the configured provider kind selects the adapter.

mod bai;
mod kilo;
mod openai_compatible;
mod opencode;

use super::types::{
    AttemptTarget, NormalizedResponse, Provider, ProviderHttpRequest, ProviderKind, ProviderRequest,
};
use crate::config::SecretString;
use reqwest::header::HeaderMap;
use reqwest::StatusCode;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("provider {0} is disabled")]
    Disabled(String),
    #[error("provider {0} has no adapter")]
    Unsupported(String),
    #[error("invalid provider request: {0}")]
    Request(String),
    #[error("credential is required for provider {0}")]
    MissingCredential(String),
    #[error("provider {provider} uses unsupported protocol {protocol:?}; this gateway currently executes OpenAI Chat Completions only")]
    UnsupportedProtocol {
        provider: String,
        protocol: super::types::ProviderProtocol,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    CredentialRejected,
    RateLimit,
    ModelUnavailable,
    ProviderServer,
    PaymentRequired,
    ClientRequest,
    Transport,
    Unknown,
}

pub trait ProviderAdapter: Send + Sync {
    fn prepare(
        &self,
        provider: &Provider,
        target: &AttemptTarget,
        request: &ProviderRequest,
        credential: Option<&SecretString>,
    ) -> Result<ProviderHttpRequest, AdapterError>;
    fn normalize(
        &self,
        provider: &Provider,
        target: &AttemptTarget,
        body: serde_json::Value,
    ) -> NormalizedResponse {
        NormalizedResponse {
            provider_id: provider.id.clone(),
            model_id: target.wire_model_id.clone(),
            body,
        }
    }

    fn classify_failure(
        &self,
        status: Option<StatusCode>,
        headers: &HeaderMap,
        body: &str,
    ) -> FailureClass {
        classify_failure(status, headers, body)
    }
}

pub struct AdapterRegistry;
impl AdapterRegistry {
    pub fn for_provider(kind: ProviderKind) -> Box<dyn ProviderAdapter> {
        match kind {
            ProviderKind::Kilo => Box::new(kilo::KiloAdapter),
            ProviderKind::OpenCode => Box::new(opencode::OpenCodeAdapter),
            ProviderKind::Bai => Box::new(bai::BaiAdapter),
            ProviderKind::OpenAiCompatible => Box::new(openai_compatible::OpenAiCompatibleAdapter),
        }
    }
}

fn bearer(
    headers: &mut std::collections::BTreeMap<String, String>,
    credential: Option<&SecretString>,
    scheme: super::types::AuthScheme,
    provider: &Provider,
) -> Result<(), AdapterError> {
    match scheme {
        super::types::AuthScheme::Bearer => {
            let value = credential
                .ok_or_else(|| AdapterError::MissingCredential(provider.id.to_string()))?;
            headers.insert(
                "Authorization".to_string(),
                format!("Bearer {}", value.expose()),
            );
        }
        super::types::AuthScheme::XApiKey => {
            let value = credential
                .ok_or_else(|| AdapterError::MissingCredential(provider.id.to_string()))?;
            headers.insert("x-api-key".to_string(), value.expose().to_string());
        }
        super::types::AuthScheme::None => {}
    }
    Ok(())
}

fn classify_failure(status: Option<StatusCode>, headers: &HeaderMap, body: &str) -> FailureClass {
    let lower = body.to_ascii_lowercase();
    let rate_limit_body = [
        "rate limit",
        "rate_limit",
        "quota exceeded",
        "quota_exceeded",
        "too many requests",
        "throttl",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if status == Some(StatusCode::TOO_MANY_REQUESTS)
        || headers.contains_key("retry-after")
        || rate_limit_body
    {
        return FailureClass::RateLimit;
    }
    match status {
        Some(StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) => FailureClass::CredentialRejected,
        Some(StatusCode::NOT_FOUND) => FailureClass::ModelUnavailable,
        Some(StatusCode::PAYMENT_REQUIRED) => FailureClass::PaymentRequired,
        Some(value) if value.is_server_error() => FailureClass::ProviderServer,
        Some(value) if value.is_client_error() => FailureClass::ClientRequest,
        Some(_) => FailureClass::Unknown,
        None => FailureClass::Transport,
    }
}

fn openai_request(
    provider: &Provider,
    target: &AttemptTarget,
    request: &ProviderRequest,
    credential: Option<&SecretString>,
    suffix: &str,
    scheme: super::types::AuthScheme,
) -> Result<ProviderHttpRequest, AdapterError> {
    if !provider.enabled {
        return Err(AdapterError::Disabled(provider.id.to_string()));
    }
    if provider.protocol != super::types::ProviderProtocol::OpenAiChatCompletions {
        return Err(AdapterError::UnsupportedProtocol {
            provider: provider.id.to_string(),
            protocol: provider.protocol,
        });
    }
    let mut headers = provider.headers.clone();
    headers.insert("Content-Type".to_string(), "application/json".to_string());
    bearer(&mut headers, credential, scheme, provider)?;
    let mut body = request.body.clone().unwrap_or_else(|| {
        serde_json::json!({
            "model": target.wire_model_id,
            "messages": request.messages,
            "max_tokens": request.max_output_tokens,
            "stream": request.stream
        })
    });
    let Some(object) = body.as_object_mut() else {
        return Err(AdapterError::Request(
            "provider request body must be a JSON object".to_string(),
        ));
    };
    object.insert(
        "model".to_string(),
        serde_json::Value::String(target.wire_model_id.clone()),
    );
    object.insert("messages".to_string(), request.messages.clone());
    object.insert(
        "stream".to_string(),
        serde_json::Value::Bool(request.stream),
    );
    if let Some(max_output_tokens) = request.max_output_tokens {
        object.insert(
            "max_tokens".to_string(),
            serde_json::Value::from(max_output_tokens),
        );
    }
    Ok(ProviderHttpRequest {
        url: format!("{}{}", provider.base_url.trim_end_matches('/'), suffix),
        headers,
        body,
    })
}
