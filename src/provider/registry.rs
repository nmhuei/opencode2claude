use super::types::*;
use crate::config::SecretString;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::RwLock;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RegistryError {
    #[error("duplicate {kind} id: {id}")]
    Duplicate { kind: &'static str, id: String },
    #[error("unknown provider: {0}")]
    UnknownProvider(String),
    #[error("unknown model {model} for provider {provider}")]
    UnknownModel { provider: String, model: String },
    #[error("unknown credential: {0}")]
    UnknownCredential(String),
    #[error("credential {credential} does not belong to provider {provider}")]
    CrossProviderCredential {
        credential: String,
        provider: String,
    },
    #[error("alias {alias} has no candidates")]
    EmptyAlias { alias: String },
    #[error("strict alias {alias} candidate {model} has unknown context metadata")]
    UnknownContext { alias: String, model: String },
    #[error("strict alias {alias} candidate {model} has only {context} context tokens")]
    InsufficientContext {
        alias: String,
        model: String,
        context: usize,
    },
}

#[derive(Debug, Clone)]
pub struct ProviderRegistry {
    providers: BTreeMap<ProviderId, Provider>,
    credentials: BTreeMap<CredentialId, Credential>,
    models: BTreeMap<(ProviderId, String), ModelInfo>,
    aliases: BTreeMap<AliasId, ModelAlias>,
}

#[derive(Debug, Clone)]
pub struct ProviderSnapshot {
    pub providers: Arc<BTreeMap<ProviderId, Provider>>,
    pub credentials: Arc<BTreeMap<CredentialId, Credential>>,
    pub models: Arc<BTreeMap<(ProviderId, String), ModelInfo>>,
    pub aliases: Arc<BTreeMap<AliasId, ModelAlias>>,
}

/// Atomic runtime boundary. A request takes one immutable snapshot; replacing
/// the handle never mutates a request already in flight.
#[derive(Debug, Clone)]
pub struct ProviderRuntimeHandle {
    current: Arc<RwLock<Arc<ProviderSnapshot>>>,
}

impl ProviderRuntimeHandle {
    pub fn load(registry: &ProviderRegistry) -> Result<Self, RegistryError> {
        Ok(Self {
            current: Arc::new(RwLock::new(Arc::new(registry.compile_snapshot()?))),
        })
    }
    pub fn snapshot(&self) -> Arc<ProviderSnapshot> {
        self.current
            .read()
            .expect("provider runtime lock poisoned")
            .clone()
    }
    pub fn replace(&self, registry: &ProviderRegistry) -> Result<(), RegistryError> {
        let next = Arc::new(registry.compile_snapshot()?);
        *self
            .current
            .write()
            .expect("provider runtime lock poisoned") = next;
        Ok(())
    }
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: BTreeMap::new(),
            credentials: BTreeMap::new(),
            models: BTreeMap::new(),
            aliases: BTreeMap::new(),
        }
    }
    pub fn providers(&self) -> impl Iterator<Item = &Provider> {
        self.providers.values()
    }
    pub fn credentials(&self) -> impl Iterator<Item = &Credential> {
        self.credentials.values()
    }
    pub fn aliases(&self) -> impl Iterator<Item = &ModelAlias> {
        self.aliases.values()
    }
    pub fn models(&self) -> impl Iterator<Item = &ModelInfo> {
        self.models.values()
    }
    pub fn provider(&self, id: impl AsRef<str>) -> Option<&Provider> {
        self.providers.get(&ProviderId::from(id.as_ref()))
    }
    pub fn model(&self, provider: impl AsRef<str>, model: impl AsRef<str>) -> Option<&ModelInfo> {
        self.models.get(&(
            ProviderId::from(provider.as_ref()),
            model.as_ref().to_string(),
        ))
    }
    pub fn alias(&self, id: impl AsRef<str>) -> Option<&ModelAlias> {
        self.aliases.get(&AliasId::from(id.as_ref()))
    }
    pub fn insert_model(&mut self, model: ModelInfo) {
        self.models
            .insert((model.provider_id.clone(), model.model_id.clone()), model);
    }
    pub fn register_provider(&mut self, provider: Provider) -> Result<(), RegistryError> {
        if self.providers.contains_key(&provider.id) {
            return Err(RegistryError::Duplicate {
                kind: "provider",
                id: provider.id.to_string(),
            });
        }
        self.providers.insert(provider.id.clone(), provider);
        Ok(())
    }
    pub fn register_credential(&mut self, credential: Credential) -> Result<(), RegistryError> {
        if self.credentials.contains_key(&credential.id) {
            return Err(RegistryError::Duplicate {
                kind: "credential",
                id: credential.id.to_string(),
            });
        }
        if !self.providers.contains_key(&credential.provider_id) {
            return Err(RegistryError::UnknownProvider(
                credential.provider_id.to_string(),
            ));
        }
        self.credentials.insert(credential.id.clone(), credential);
        Ok(())
    }
    pub fn register_alias(&mut self, alias: ModelAlias) -> Result<(), RegistryError> {
        if self.aliases.contains_key(&alias.id) {
            return Err(RegistryError::Duplicate {
                kind: "alias",
                id: alias.id.to_string(),
            });
        }
        self.validate_alias(&alias)?;
        self.aliases.insert(alias.id.clone(), alias);
        Ok(())
    }
    pub fn compile_snapshot(&self) -> Result<ProviderSnapshot, RegistryError> {
        for alias in self.aliases.values() {
            self.validate_alias(alias)?;
        }
        Ok(ProviderSnapshot {
            providers: Arc::new(self.providers.clone()),
            credentials: Arc::new(self.credentials.clone()),
            models: Arc::new(self.models.clone()),
            aliases: Arc::new(self.aliases.clone()),
        })
    }
    pub fn resolve_alias(&self, id: impl AsRef<str>) -> Result<Vec<AttemptTarget>, RegistryError> {
        let alias = self
            .alias(id.as_ref())
            .ok_or_else(|| RegistryError::UnknownModel {
                provider: "alias".to_string(),
                model: id.as_ref().to_string(),
            })?;
        let mut candidates: Vec<_> = alias.candidates.iter().enumerate().collect();
        candidates.sort_by_key(|(_, candidate)| candidate.priority);
        candidates
            .into_iter()
            .enumerate()
            .map(|(candidate_index, (_, candidate))| {
                let info = self
                    .models
                    .get(&(candidate.provider_id.clone(), candidate.model_id.clone()))
                    .ok_or_else(|| RegistryError::UnknownModel {
                        provider: candidate.provider_id.to_string(),
                        model: candidate.model_id.clone(),
                    })?;
                Ok(AttemptTarget {
                    alias_id: alias.id.clone(),
                    client_model: alias.client_model.clone(),
                    provider_id: info.provider_id.clone(),
                    model_id: info.model_id.clone(),
                    wire_model_id: info.wire_model_id.clone(),
                    credential_id: candidate.credential_id.clone(),
                    auth_scheme: candidate
                        .credential_id
                        .as_ref()
                        .and_then(|id| {
                            self.credentials
                                .get(id)
                                .map(|credential| credential.auth_scheme)
                        })
                        .unwrap_or(AuthScheme::None),
                    context_window: info.context_window.unwrap_or_default(),
                    candidate_index,
                })
            })
            .collect()
    }
    pub fn from_legacy(
        base_url: impl Into<String>,
        model: impl Into<String>,
        credentials: Option<Vec<SecretString>>,
    ) -> Self {
        let mut out = Self::new();
        let provider_id = ProviderId::from("legacy");
        let _ = out.register_provider(Provider {
            id: provider_id.clone(),
            name: "Legacy upstream".to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: base_url.into(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            headers: BTreeMap::new(),
            enabled: true,
        });
        let credential_id = credentials
            .as_ref()
            .and_then(|values| (!values.is_empty()).then(|| CredentialId::from("legacy-main")));
        if let Some(id) = &credential_id {
            let _ = out.register_credential(Credential {
                id: id.clone(),
                provider_id: provider_id.clone(),
                source: SecretSource::Managed { id: id.to_string() },
                auth_scheme: AuthScheme::Bearer,
            });
        }
        let model = model.into();
        out.insert_model(ModelInfo {
            provider_id: provider_id.clone(),
            model_id: model.clone(),
            wire_model_id: model.clone(),
            context_window: None,
            max_output_tokens: None,
            supports_thinking: false,
            verified_context: false,
            free: false,
        });
        let _ = out.register_alias(ModelAlias {
            id: AliasId::from("default"),
            client_model: "claude-sonnet-5".to_string(),
            context_window: 128_000,
            strict_context: false,
            candidates: vec![ModelCandidate {
                provider_id,
                model_id: model,
                credential_id,
                priority: 0,
            }],
        });
        out
    }
    fn validate_alias(&self, alias: &ModelAlias) -> Result<(), RegistryError> {
        if alias.candidates.is_empty() {
            return Err(RegistryError::EmptyAlias {
                alias: alias.id.to_string(),
            });
        }
        for candidate in &alias.candidates {
            if !self.providers.contains_key(&candidate.provider_id) {
                return Err(RegistryError::UnknownProvider(
                    candidate.provider_id.to_string(),
                ));
            }
            let info = self
                .models
                .get(&(candidate.provider_id.clone(), candidate.model_id.clone()))
                .ok_or_else(|| RegistryError::UnknownModel {
                    provider: candidate.provider_id.to_string(),
                    model: candidate.model_id.clone(),
                })?;
            if let Some(credential) = &candidate.credential_id {
                let credential_obj = self
                    .credentials
                    .get(credential)
                    .ok_or_else(|| RegistryError::UnknownCredential(credential.to_string()))?;
                if credential_obj.provider_id != candidate.provider_id {
                    return Err(RegistryError::CrossProviderCredential {
                        credential: credential.to_string(),
                        provider: candidate.provider_id.to_string(),
                    });
                }
            }
            if alias.strict_context {
                match info.context_window {
                    Some(value) if value >= alias.context_window && info.verified_context => {}
                    Some(value) => {
                        return Err(RegistryError::InsufficientContext {
                            alias: alias.id.to_string(),
                            model: candidate.model_id.clone(),
                            context: value,
                        })
                    }
                    None => {
                        return Err(RegistryError::UnknownContext {
                            alias: alias.id.to_string(),
                            model: candidate.model_id.clone(),
                        })
                    }
                }
            }
        }
        Ok(())
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn provider(id: &str) -> Provider {
        Provider {
            id: id.into(),
            name: id.into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://example.test/v1".into(),
            protocol: ProviderProtocol::OpenAiChatCompletions,
            headers: BTreeMap::new(),
            enabled: true,
        }
    }
    fn model(provider: &str, id: &str, context: Option<usize>) -> ModelInfo {
        ModelInfo {
            provider_id: provider.into(),
            model_id: id.into(),
            wire_model_id: id.into(),
            context_window: context,
            max_output_tokens: None,
            supports_thinking: false,
            verified_context: context.is_some(),
            free: true,
        }
    }
    #[test]
    fn identical_model_ids_from_two_providers_remain_distinct() {
        let mut r = ProviderRegistry::new();
        r.register_provider(provider("bai")).unwrap();
        r.register_provider(provider("kilo")).unwrap();
        r.insert_model(model("bai", "model-x", Some(1_000_000)));
        r.insert_model(model("kilo", "model-x", Some(128_000)));
        assert_eq!(
            r.model("bai", "model-x").unwrap().context_window,
            Some(1_000_000)
        );
        assert_eq!(
            r.model("kilo", "model-x").unwrap().context_window,
            Some(128_000)
        );
    }
    #[test]
    fn strict_alias_rejects_unknown_or_small_context() {
        let mut r = ProviderRegistry::new();
        r.register_provider(provider("bai")).unwrap();
        r.insert_model(model("bai", "small", Some(128_000)));
        let err = r
            .register_alias(ModelAlias::one_million(
                "free-1m",
                vec![ModelCandidate {
                    provider_id: "bai".into(),
                    model_id: "small".into(),
                    credential_id: None,
                    priority: 0,
                }],
            ))
            .unwrap_err();
        assert!(matches!(err, RegistryError::InsufficientContext { .. }));
    }
    #[test]
    fn credentials_are_provider_scoped() {
        let mut r = ProviderRegistry::new();
        r.register_provider(provider("bai")).unwrap();
        r.register_provider(provider("kilo")).unwrap();
        r.register_credential(Credential {
            id: "bai-key".into(),
            provider_id: "bai".into(),
            source: SecretSource::Managed {
                id: "bai-key".into(),
            },
            auth_scheme: AuthScheme::Bearer,
        })
        .unwrap();
        r.insert_model(model("kilo", "x", Some(1_000_000)));
        let err = r
            .register_alias(ModelAlias::one_million(
                "x",
                vec![ModelCandidate {
                    provider_id: "kilo".into(),
                    model_id: "x".into(),
                    credential_id: Some("bai-key".into()),
                    priority: 0,
                }],
            ))
            .unwrap_err();
        assert!(matches!(err, RegistryError::CrossProviderCredential { .. }));
    }
}
