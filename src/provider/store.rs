//! Atomic, validated persistence for the provider registry.

use super::config::{load_provider_registry, ProviderConfigError};
use super::registry::{ProviderRegistry, RegistryError};
use super::types::{
    AliasId, AuthScheme, Credential, CredentialPool, ModelAlias, ModelInfo, PoolStrategy, Provider,
    SecretSource,
};
use crate::infrastructure::file_store::{AtomicFileStore, FileStore};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProviderStoreError {
    #[error("provider config file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("provider config error: {0}")]
    Config(#[from] ProviderConfigError),
    #[error("provider mutation failed: {0}")]
    Registry(#[from] RegistryError),
    #[error("provider config serialization failed: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("provider config document error: {0}")]
    Document(String),
}

#[derive(Debug, Clone)]
pub enum ProviderMutation {
    AddProvider(Provider),
    SetCredential(Credential),
    UpsertPool(CredentialPool),
    RemovePool(String),
    UpsertModel(ModelInfo),
    SetAlias(ModelAlias),
    EnableProvider { id: String, enabled: bool },
    RemoveProvider(String),
    RemoveCredential(String),
    RemoveModel { provider: String, model: String },
    RemoveAlias(String),
    SetActiveAlias(Option<AliasId>),
}

#[derive(Debug, Clone)]
pub struct StoreMigrationReport {
    pub from_version: u32,
    pub to_version: u32,
    pub backup_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ProviderConfigStore {
    path: PathBuf,
}

impl ProviderConfigStore {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<ProviderRegistry, ProviderStoreError> {
        if !self.path.exists() {
            return Ok(ProviderRegistry::new());
        }
        Ok(load_provider_registry(&self.path)?)
    }

    pub fn transaction(
        &self,
        mutation: ProviderMutation,
    ) -> Result<ProviderRegistry, ProviderStoreError> {
        let mut registry = self.load()?;
        apply_mutation(&mut registry, mutation)?;
        registry.compile_snapshot()?;
        let rendered = render_v3_registry(&registry)?;
        let document = merge_provider_document(&self.path, &rendered)?;
        AtomicFileStore.atomic_write(&self.path, document.as_bytes(), true)?;
        Ok(registry)
    }

    pub fn migrate_to_v3(&self) -> Result<StoreMigrationReport, ProviderStoreError> {
        let original = std::fs::read(&self.path)?;
        let from_version = std::str::from_utf8(&original)
            .ok()
            .and_then(|raw| raw.parse::<toml::Value>().ok())
            .and_then(|value| {
                value
                    .get("schema_version")
                    .and_then(toml::Value::as_integer)
            })
            .unwrap_or(0) as u32;
        let registry = self.load()?;
        let rendered = render_v3_registry(&registry)?;
        let document = merge_provider_document(&self.path, &rendered)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let backup_path = self.path.with_file_name(format!(
            "{}.v{}-backup-{stamp}",
            file_name(&self.path),
            from_version
        ));
        AtomicFileStore.atomic_write(&backup_path, &original, true)?;
        AtomicFileStore.atomic_write(&self.path, document.as_bytes(), true)?;
        Ok(StoreMigrationReport {
            from_version,
            to_version: 3,
            backup_path,
        })
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("provider-config")
        .to_string()
}

/// Provider management shares the bridge TOML file with unrelated runtime
/// settings. Replace only provider-owned keys so adding an endpoint cannot
/// silently discard ports, auth policy, proxy settings, or comments.
fn merge_provider_document(path: &Path, rendered: &str) -> Result<String, ProviderStoreError> {
    let mut document = if path.exists() {
        std::fs::read_to_string(path)
            .map_err(ProviderStoreError::Io)?
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ProviderStoreError::Document(error.to_string()))?
    } else {
        toml_edit::DocumentMut::new()
    };
    let generated = rendered
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| ProviderStoreError::Document(error.to_string()))?;
    for key in [
        "schema_version",
        "router",
        "providers",
        "credentials",
        "models",
        "aliases",
        "credential_pools",
    ] {
        document.remove(key);
        if let Some(item) = generated.get(key) {
            document[key] = item.clone();
        }
    }
    Ok(document.to_string())
}

fn apply_mutation(
    registry: &mut ProviderRegistry,
    mutation: ProviderMutation,
) -> Result<(), RegistryError> {
    match mutation {
        ProviderMutation::AddProvider(provider) => registry.register_provider(provider),
        ProviderMutation::SetCredential(credential) => registry.upsert_credential(credential),
        ProviderMutation::UpsertPool(pool) => registry.upsert_pool(pool),
        ProviderMutation::RemovePool(id) => registry.remove_pool(id),
        ProviderMutation::UpsertModel(model) => {
            if registry.provider(&model.provider_id).is_none() {
                return Err(RegistryError::UnknownProvider(
                    model.provider_id.to_string(),
                ));
            }
            registry.insert_model(model);
            Ok(())
        }
        ProviderMutation::SetAlias(alias) => registry.upsert_alias(alias),
        ProviderMutation::EnableProvider { id, enabled } => registry.set_enabled(id, enabled),
        ProviderMutation::RemoveProvider(id) => registry.remove_provider(id),
        ProviderMutation::RemoveCredential(id) => registry.remove_credential(id),
        ProviderMutation::RemoveModel { provider, model } => registry.remove_model(provider, model),
        ProviderMutation::RemoveAlias(id) => registry.remove_alias(id),
        ProviderMutation::SetActiveAlias(alias) => {
            if let Some(alias_id) = &alias {
                if registry.alias(alias_id).is_none() {
                    return Err(RegistryError::UnknownModel {
                        provider: "alias".to_string(),
                        model: alias_id.to_string(),
                    });
                }
            }
            registry.set_active_alias(alias);
            Ok(())
        }
    }
}

#[derive(Debug, Serialize)]
struct V3File<'a> {
    schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    router: Option<V3Router<'a>>,
    providers: BTreeMap<String, V3Provider<'a>>,
    credentials: BTreeMap<String, V3Credential<'a>>,
    models: BTreeMap<String, BTreeMap<String, V3Model<'a>>>,
    aliases: BTreeMap<String, V3Alias<'a>>,
    credential_pools: BTreeMap<String, V3Pool<'a>>,
}

#[derive(Debug, Serialize)]
struct V3Router<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    active_alias: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct V3Provider<'a> {
    name: &'a str,
    kind: super::types::ProviderKind,
    base_url: &'a str,
    protocol: super::types::ProviderProtocol,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    headers: &'a BTreeMap<String, String>,
    enabled: bool,
}

#[derive(Debug, Serialize)]
struct V3Credential<'a> {
    provider: &'a str,
    source: String,
    auth_scheme: AuthScheme,
}

#[derive(Debug, Serialize)]
struct V3Model<'a> {
    wire_model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_window: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<usize>,
    supports_thinking: bool,
    verified_context: bool,
    free: bool,
}

#[derive(Debug, Serialize)]
struct V3Alias<'a> {
    client_model: &'a str,
    context_window: usize,
    strict_context: bool,
    candidates: Vec<V3Candidate<'a>>,
}

#[derive(Debug, Serialize)]
struct V3Candidate<'a> {
    provider: &'a str,
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    credential: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    credential_pool: Option<&'a str>,
    priority: i32,
}

#[derive(Debug, Serialize)]
struct V3Pool<'a> {
    provider: &'a str,
    strategy: PoolStrategy,
    members: Vec<V3PoolMember<'a>>,
}

#[derive(Debug, Serialize)]
struct V3PoolMember<'a> {
    credential: &'a str,
    quota_scope: &'a str,
    max_in_flight: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    requests_per_minute: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens_per_minute: Option<u64>,
    weight: u32,
}

pub(crate) fn render_v3_registry(registry: &ProviderRegistry) -> Result<String, toml::ser::Error> {
    let providers = registry
        .providers()
        .map(|provider| {
            (
                provider.id.to_string(),
                V3Provider {
                    name: &provider.name,
                    kind: provider.kind,
                    base_url: &provider.base_url,
                    protocol: provider.protocol,
                    headers: &provider.headers,
                    enabled: provider.enabled,
                },
            )
        })
        .collect();
    let credentials = registry
        .credentials()
        .map(|credential| {
            (
                credential.id.to_string(),
                V3Credential {
                    provider: credential.provider_id.as_ref(),
                    source: source_string(&credential.source),
                    auth_scheme: credential.auth_scheme,
                },
            )
        })
        .collect();
    let mut models = BTreeMap::new();
    for model in registry.models() {
        models
            .entry(model.provider_id.to_string())
            .or_insert_with(BTreeMap::new)
            .insert(
                model.model_id.clone(),
                V3Model {
                    wire_model: &model.wire_model_id,
                    context_window: model.context_window,
                    max_output_tokens: model.max_output_tokens,
                    supports_thinking: model.supports_thinking,
                    verified_context: model.verified_context,
                    free: model.free,
                },
            );
    }
    let aliases = registry
        .aliases()
        .map(|alias| {
            (
                alias.id.to_string(),
                V3Alias {
                    client_model: &alias.client_model,
                    context_window: alias.context_window,
                    strict_context: alias.strict_context,
                    candidates: alias
                        .candidates
                        .iter()
                        .map(|candidate| V3Candidate {
                            provider: candidate.provider_id.as_ref(),
                            model: &candidate.model_id,
                            credential: candidate.credential_id.as_ref().map(|id| id.as_ref()),
                            credential_pool: candidate
                                .credential_pool_id
                                .as_ref()
                                .map(|id| id.as_ref()),
                            priority: candidate.priority,
                        })
                        .collect(),
                },
            )
        })
        .collect();
    let credential_pools = registry
        .pools()
        .map(|pool| {
            (
                pool.id.to_string(),
                V3Pool {
                    provider: pool.provider_id.as_ref(),
                    strategy: pool.strategy,
                    members: pool
                        .members
                        .iter()
                        .map(|member| V3PoolMember {
                            credential: member.credential_id.as_ref(),
                            quota_scope: &member.quota_scope,
                            max_in_flight: member.max_in_flight.get(),
                            requests_per_minute: member
                                .requests_per_minute
                                .map(|value| value.get()),
                            tokens_per_minute: member.tokens_per_minute.map(|value| value.get()),
                            weight: member.weight.get(),
                        })
                        .collect(),
                },
            )
        })
        .collect();
    let router = registry.active_alias().map(|alias| V3Router {
        active_alias: Some(alias.as_ref()),
    });
    toml::to_string_pretty(&V3File {
        schema_version: 3,
        router,
        providers,
        credentials,
        models,
        aliases,
        credential_pools,
    })
}

fn source_string(source: &SecretSource) -> String {
    match source {
        SecretSource::Env { variable } => format!("env:{variable}"),
        SecretSource::File { path } => format!("file:{path}"),
        SecretSource::Managed { id } => format!("managed:{id}"),
    }
}
