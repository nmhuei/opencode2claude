use crate::config::SecretString;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);
        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }
        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_type!(ProviderId);
id_type!(CredentialId);
id_type!(AliasId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    #[serde(rename = "opencode", alias = "open-code")]
    OpenCode,
    #[serde(rename = "kilo")]
    Kilo,
    #[serde(rename = "bai", alias = "b.ai")]
    Bai,
    #[serde(rename = "openai-compatible", alias = "open-ai-compatible")]
    OpenAiCompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    #[serde(rename = "openai_chat_completions", alias = "open_ai_chat_completions")]
    OpenAiChatCompletions,
    #[serde(rename = "openai_responses", alias = "open_ai_responses")]
    OpenAiResponses,
    AnthropicMessages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AuthScheme {
    #[serde(rename = "bearer")]
    #[default]
    Bearer,
    #[serde(rename = "x-api-key", alias = "x_api_key")]
    XApiKey,
    None,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Provider {
    pub id: ProviderId,
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub protocol: ProviderProtocol,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl fmt::Debug for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Provider")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("protocol", &self.protocol)
            .field("headers", &self.headers)
            .field("enabled", &self.enabled)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Credential {
    pub id: CredentialId,
    pub provider_id: ProviderId,
    pub source: SecretSource,
    pub auth_scheme: AuthScheme,
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credential")
            .field("id", &self.id)
            .field("provider_id", &self.provider_id)
            .field("source", &self.source)
            .field("auth_scheme", &self.auth_scheme)
            .finish()
    }
}

impl Credential {
    pub fn belongs_to(&self, provider: impl AsRef<str>) -> Result<(), String> {
        (self.provider_id.as_ref() == provider.as_ref())
            .then_some(())
            .ok_or_else(|| {
                format!(
                    "credential {} belongs to provider {}, not {}",
                    self.id,
                    self.provider_id,
                    provider.as_ref()
                )
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SecretSource {
    Env { variable: String },
    File { path: String },
    Managed { id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub provider_id: ProviderId,
    pub model_id: String,
    pub wire_model_id: String,
    pub context_window: Option<usize>,
    pub max_output_tokens: Option<usize>,
    #[serde(default)]
    pub supports_thinking: bool,
    #[serde(default)]
    pub verified_context: bool,
    #[serde(default)]
    pub free: bool,
}

impl ModelInfo {
    pub fn context_class(&self) -> ContextClass {
        match self.context_window {
            Some(value) if value >= 1_000_000 => ContextClass::OneMillion,
            Some(value) => ContextClass::Known(value),
            None => ContextClass::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextClass {
    OneMillion,
    Known(usize),
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCandidate {
    pub provider_id: ProviderId,
    pub model_id: String,
    pub credential_id: Option<CredentialId>,
    #[serde(default)]
    pub priority: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAlias {
    pub id: AliasId,
    pub client_model: String,
    pub context_window: usize,
    #[serde(default)]
    pub strict_context: bool,
    pub candidates: Vec<ModelCandidate>,
}

impl ModelAlias {
    pub fn one_million(id: impl Into<AliasId>, candidates: Vec<ModelCandidate>) -> Self {
        Self {
            id: id.into(),
            client_model: "claude-sonnet-5[1m]".to_string(),
            context_window: 1_000_000,
            strict_context: true,
            candidates,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AttemptTarget {
    pub alias_id: AliasId,
    pub client_model: String,
    pub provider_id: ProviderId,
    pub model_id: String,
    pub wire_model_id: String,
    pub credential_id: Option<CredentialId>,
    pub auth_scheme: AuthScheme,
    pub context_window: usize,
    pub candidate_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRequest {
    pub client_model: String,
    pub messages: serde_json::Value,
    pub max_output_tokens: Option<usize>,
    pub stream: bool,
}

#[derive(Debug, Clone)]
pub struct ProviderHttpRequest {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct NormalizedResponse {
    pub provider_id: ProviderId,
    pub model_id: String,
    pub body: serde_json::Value,
}

#[allow(dead_code)]
pub(crate) fn redacted_secret(secret: &SecretString) -> String {
    let _ = secret;
    "[REDACTED]".to_string()
}
