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
    #[error(
        "credential {credential} has invalid source; use env:NAME, file:PATH, or managed[:ID]"
    )]
    InvalidSecretSource { credential: String },
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProviderFileConfig {
    pub schema_version: Option<u32>,
    pub active_alias: Option<String>,
    #[serde(default)]
    pub providers: Vec<ProviderEntry>,
    #[serde(default)]
    pub credentials: Vec<CredentialEntry>,
    #[serde(default)]
    pub models: Vec<ModelEntry>,
    #[serde(default)]
    pub aliases: Vec<AliasFileConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ProviderFileV3 {
    schema_version: u32,
    #[serde(default)]
    router: RouterFileConfig,
    #[serde(default)]
    providers: BTreeMap<String, ProviderV3Entry>,
    #[serde(default)]
    credentials: BTreeMap<String, CredentialV3Entry>,
    #[serde(default)]
    models: BTreeMap<String, BTreeMap<String, ModelV3Entry>>,
    #[serde(default)]
    aliases: BTreeMap<String, AliasV3Entry>,
    #[serde(default)]
    credential_pools: BTreeMap<String, CredentialPoolV3Entry>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct RouterFileConfig {
    active_alias: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProviderV3Entry {
    #[serde(default)]
    name: String,
    kind: ProviderKind,
    base_url: String,
    #[serde(default = "default_protocol")]
    protocol: ProviderProtocol,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct CredentialV3Entry {
    provider: String,
    source: String,
    #[serde(default)]
    auth_scheme: AuthScheme,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ModelV3Entry {
    wire_model: Option<String>,
    context_window: Option<usize>,
    max_output_tokens: Option<usize>,
    #[serde(default)]
    supports_thinking: bool,
    #[serde(default)]
    verified_context: bool,
    #[serde(default)]
    free: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct AliasV3Entry {
    client_model: String,
    context_window: usize,
    #[serde(default)]
    strict_context: bool,
    #[serde(default)]
    candidates: Vec<CandidateV3Entry>,
}

#[derive(Debug, Clone, Deserialize)]
struct CandidateV3Entry {
    provider: String,
    model: String,
    #[serde(default)]
    credential: Option<String>,
    #[serde(default)]
    credential_pool: Option<String>,
    #[serde(default)]
    priority: i32,
}

#[derive(Debug, Clone, Deserialize)]
struct CredentialPoolV3Entry {
    provider: String,
    #[serde(default)]
    strategy: PoolStrategy,
    #[serde(default)]
    members: Vec<CredentialPoolMemberV3Entry>,
}

#[derive(Debug, Clone, Deserialize)]
struct CredentialPoolMemberV3Entry {
    credential: String,
    #[serde(default)]
    quota_scope: Option<String>,
    #[serde(default = "default_pool_max_in_flight")]
    max_in_flight: u32,
    #[serde(default)]
    requests_per_minute: Option<u32>,
    #[serde(default)]
    tokens_per_minute: Option<u64>,
    #[serde(default = "default_pool_weight")]
    weight: u32,
}

fn default_pool_max_in_flight() -> u32 {
    1
}

fn default_pool_weight() -> u32 {
    1
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
            validate_provider_url(&entry.id, &entry.base_url)?;
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
                        credential_pool_id: None,
                        priority: candidate.priority,
                    })
                    .collect(),
            })?;
        }
        registry.set_active_alias(self.active_alias.map(AliasId::from));
        registry.compile_snapshot()?;
        Ok(registry)
    }
}

impl ProviderFileV3 {
    fn into_registry(self) -> Result<ProviderRegistry, ProviderConfigError> {
        if self.schema_version != 3 {
            return Err(ProviderConfigError::Schema(self.schema_version));
        }
        let mut registry = ProviderRegistry::new();
        for (id, entry) in self.providers {
            validate_provider_url(&id, &entry.base_url)?;
            registry.register_provider(Provider {
                id: id.clone().into(),
                name: if entry.name.is_empty() {
                    id
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
        for (id, entry) in self.credentials {
            let source = parse_v3_secret_source(&id, &entry.source)?;
            registry.register_credential(Credential {
                id: id.into(),
                provider_id: entry.provider.into(),
                source,
                auth_scheme: entry.auth_scheme,
            })?;
        }
        for (provider_id, models) in self.models {
            for (model_id, entry) in models {
                let wire_model_id = entry.wire_model.unwrap_or_else(|| model_id.clone());
                registry.insert_model(ModelInfo {
                    provider_id: provider_id.clone().into(),
                    model_id,
                    wire_model_id,
                    context_window: entry.context_window,
                    max_output_tokens: entry.max_output_tokens,
                    supports_thinking: entry.supports_thinking,
                    verified_context: entry.verified_context,
                    free: entry.free,
                });
            }
        }
        for (id, entry) in self.credential_pools {
            let members = entry
                .members
                .into_iter()
                .map(|member| {
                    let credential = member.credential;
                    let credential_id = CredentialId::from(credential.clone());
                    let quota_scope = member
                        .quota_scope
                        .unwrap_or_else(|| format!("credential:{credential_id}"));
                    CredentialPoolMember::new(
                        credential_id,
                        quota_scope,
                        member.max_in_flight,
                        member.requests_per_minute,
                        member.tokens_per_minute,
                        member.weight,
                    )
                    .map_err(|reason| RegistryError::InvalidPoolMember {
                        pool: id.clone(),
                        credential,
                        reason,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let pool = CredentialPool::new(id.clone(), entry.provider, entry.strategy, members)
                .map_err(|reason| RegistryError::InvalidPool {
                    pool: id.clone(),
                    reason,
                })?;
            registry.upsert_pool(pool)?;
        }
        for (id, entry) in self.aliases {
            registry.register_alias(ModelAlias {
                id: id.into(),
                client_model: entry.client_model,
                context_window: entry.context_window,
                strict_context: entry.strict_context,
                candidates: entry
                    .candidates
                    .into_iter()
                    .map(|candidate| {
                        let pool = candidate.credential_pool.map(|pool| {
                            CredentialPoolId::new(pool).map_err(|reason| {
                                RegistryError::InvalidPool {
                                    pool: "candidate".to_string(),
                                    reason,
                                }
                            })
                        });
                        Ok(ModelCandidate {
                            provider_id: candidate.provider.into(),
                            model_id: candidate.model,
                            credential_id: candidate.credential.map(Into::into),
                            credential_pool_id: pool.transpose()?,
                            priority: candidate.priority,
                        })
                    })
                    .collect::<Result<Vec<_>, ProviderConfigError>>()?,
            })?;
        }
        registry.set_active_alias(self.router.active_alias.map(AliasId::from));
        registry.compile_snapshot()?;
        Ok(registry)
    }
}

fn validate_provider_url(provider: &str, value: &str) -> Result<(), ProviderConfigError> {
    let parsed = reqwest::Url::parse(value).map_err(|_| ProviderConfigError::InvalidUrl {
        provider: provider.to_string(),
        url: value.to_string(),
    })?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(ProviderConfigError::InvalidUrl {
            provider: provider.to_string(),
            url: value.to_string(),
        });
    }
    Ok(())
}

fn parse_v3_secret_source(id: &str, source: &str) -> Result<SecretSource, ProviderConfigError> {
    let source = source.trim();
    let (kind, value) = source.split_once(':').unwrap_or((source, ""));
    match kind {
        "env" if !value.trim().is_empty() => Ok(SecretSource::Env {
            variable: value.trim().to_string(),
        }),
        "file" if !value.trim().is_empty() => Ok(SecretSource::File {
            path: value.trim().to_string(),
        }),
        "managed" if value.trim().is_empty() => Ok(SecretSource::Managed { id: id.to_string() }),
        "managed" if !value.trim().is_empty() => Ok(SecretSource::Managed {
            id: value.trim().to_string(),
        }),
        _ => Err(ProviderConfigError::InvalidSecretSource {
            credential: id.to_string(),
        }),
    }
}

pub fn load_provider_registry(config_path: &Path) -> Result<ProviderRegistry, ProviderConfigError> {
    let raw = std::fs::read_to_string(config_path)?;
    let version = raw
        .parse::<toml::Value>()?
        .get("schema_version")
        .and_then(toml::Value::as_integer)
        .unwrap_or(0);
    match version {
        3 => Ok(toml::from_str::<ProviderFileV3>(&raw)?.into_registry()?),
        0..=2 => toml::from_str::<ProviderFileConfig>(&raw)?.into_registry(),
        value => Err(ProviderConfigError::Schema(value as u32)),
    }
}

pub fn schema_v2_example() -> &'static str {
    "schema_version = 2\n\n[[providers]]\nid = \"bai\"\nname = \"B.AI\"\nkind = \"bai\"\nbase_url = \"https://api.b.ai/v1\"\n\n[[credentials]]\nid = \"bai-main\"\nprovider_id = \"bai\"\nenv = \"BAI_API_KEY\"\n\n[[models]]\nprovider_id = \"bai\"\nmodel_id = \"deepseek-1m\"\ncontext_window = 1000000\nverified_context = true\n\n[[aliases]]\nid = \"free-1m\"\nclient_model = \"sonnet[1m]\"\ncontext_window = 1000000\nstrict_context = true\n\n[[aliases.candidates]]\nprovider_id = \"bai\"\nmodel_id = \"deepseek-1m\"\ncredential_id = \"bai-main\"\npriority = 0\n"
}
