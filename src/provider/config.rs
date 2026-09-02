//! Versioned provider configuration and legacy conversion.

use super::registry::{ProviderRegistry, RegistryError};
use super::types::*;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderConfigError {
    #[error("cannot read provider config: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid provider TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("provider registry validation failed: {0}")]
    Registry(#[from] RegistryError),
    #[error("schema version {0} is not supported")]
    Schema(u32),
    #[error("provider {provider} has invalid base URL: {url}")]
    InvalidUrl { provider: String, url: String },
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProviderFileConfig {
    pub schema_version: Option<u32>,
    #[serde(default)]
    pub providers: Vec<ProviderEntry>,
    #[serde(default)]
    pub credentials: Vec<CredentialEntry>,
    #[serde(default)]
    pub models: Vec<ModelEntry>,
    #[serde(default)]
    pub aliases: Vec<AliasFileConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderEntry {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    #[serde(default = "default_protocol")]
    pub protocol: ProviderProtocol,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}
fn default_true() -> bool {
    true
}
fn default_protocol() -> ProviderProtocol {
    ProviderProtocol::OpenAiChatCompletions
}

#[derive(Debug, Clone, Deserialize)]
pub struct CredentialEntry {
    pub id: String,
    pub provider_id: String,
    pub env: Option<String>,
    pub file: Option<String>,
    pub managed: Option<String>,
    #[serde(default)]
    pub auth_scheme: AuthScheme,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelEntry {
    pub provider_id: String,
    pub model_id: String,
    pub wire_model_id: Option<String>,
    pub context_window: Option<usize>,
    pub max_output_tokens: Option<usize>,
    #[serde(default)]
    pub supports_thinking: bool,
    #[serde(default)]
    pub verified_context: bool,
    #[serde(default)]
    pub free: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AliasFileConfig {
    pub id: String,
    pub client_model: String,
    pub context_window: usize,
    #[serde(default)]
    pub strict_context: bool,
    pub candidates: Vec<CandidateFileConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CandidateFileConfig {
    pub provider_id: String,
    pub model_id: String,
    pub credential_id: Option<String>,
    #[serde(default)]
    pub priority: i32,
}

impl ProviderFileConfig {
    pub fn into_registry(self) -> Result<ProviderRegistry, ProviderConfigError> {
        let mut registry = ProviderRegistry::new();
        for entry in self.providers {
            let parsed = reqwest::Url::parse(&entry.base_url).map_err(|_| {
                ProviderConfigError::InvalidUrl {
                    provider: entry.id.clone(),
                    url: entry.base_url.clone(),
                }
            })?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                return Err(ProviderConfigError::InvalidUrl {
                    provider: entry.id.clone(),
                    url: entry.base_url,
                });
            }
            registry.register_provider(Provider {
                id: entry.id.clone().into(),
                name: if entry.name.is_empty() {
                    entry.id
                } else {
                    entry.name
                },
                kind: entry.kind,
                base_url: entry.base_url,
                protocol: entry.protocol,
                headers: entry.headers,
                enabled: entry.enabled,
            })?;
        }
        for entry in self.credentials {
            let source = match (entry.env, entry.file, entry.managed) {
                (Some(variable), None, None) => SecretSource::Env { variable },
                (None, Some(path), None) => SecretSource::File { path },
                (None, None, Some(id)) => SecretSource::Managed { id },
                _ => return Err(ProviderConfigError::Schema(2)),
            };
            registry.register_credential(Credential {
                id: entry.id.into(),
                provider_id: entry.provider_id.into(),
                source,
                auth_scheme: entry.auth_scheme,
            })?;
        }
        for entry in self.models {
            let wire = entry
                .wire_model_id
                .unwrap_or_else(|| entry.model_id.clone());
            registry.insert_model(ModelInfo {
                provider_id: entry.provider_id.into(),
                model_id: entry.model_id,
                wire_model_id: wire,
                context_window: entry.context_window,
                max_output_tokens: entry.max_output_tokens,
                supports_thinking: entry.supports_thinking,
                verified_context: entry.verified_context,
                free: entry.free,
            });
        }
        for entry in self.aliases {
            registry.register_alias(ModelAlias {
                id: entry.id.into(),
                client_model: entry.client_model,
                context_window: entry.context_window,
                strict_context: entry.strict_context,
                candidates: entry
                    .candidates
                    .into_iter()
                    .map(|candidate| ModelCandidate {
                        provider_id: candidate.provider_id.into(),
                        model_id: candidate.model_id,
                        credential_id: candidate.credential_id.map(Into::into),
                        priority: candidate.priority,
                    })
                    .collect(),
            })?;
        }
        registry.compile_snapshot()?;
        Ok(registry)
    }
}

pub fn load_provider_registry(config_path: &Path) -> Result<ProviderRegistry, ProviderConfigError> {
    let raw = std::fs::read_to_string(config_path)?;
    let config: ProviderFileConfig = toml::from_str(&raw)?;
    if let Some(version) = config.schema_version {
        if version != 2 {
            return Err(ProviderConfigError::Schema(version));
        }
    }
    config.into_registry()
}

pub fn schema_v2_example() -> &'static str {
    "schema_version = 2\n\n[[providers]]\nid = \"bai\"\nname = \"B.AI\"\nkind = \"bai\"\nbase_url = \"https://api.b.ai/v1\"\n\n[[credentials]]\nid = \"bai-main\"\nprovider_id = \"bai\"\nenv = \"BAI_API_KEY\"\n\n[[models]]\nprovider_id = \"bai\"\nmodel_id = \"deepseek-1m\"\ncontext_window = 1000000\nverified_context = true\n\n[[aliases]]\nid = \"free-1m\"\nclient_model = \"sonnet[1m]\"\ncontext_window = 1000000\nstrict_context = true\n\n[[aliases.candidates]]\nprovider_id = \"bai\"\nmodel_id = \"deepseek-1m\"\ncredential_id = \"bai-main\"\npriority = 0\n"
}
