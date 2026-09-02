use super::capacity::CapacityScheduler;
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
    #[error("unknown credential pool: {0}")]
    UnknownPool(String),
    #[error("credential {credential} does not belong to provider {provider}")]
    CrossProviderCredential {
        credential: String,
        provider: String,
    },
    #[error("credential pool {pool} is invalid: {reason}")]
    InvalidPool { pool: String, reason: String },
    #[error("credential {credential} in pool {pool} is invalid: {reason}")]
    InvalidPoolMember {
        pool: String,
        credential: String,
        reason: String,
    },
    #[error("credential {credential} is already assigned to pools {first_pool} and {second_pool}")]
    CredentialInMultiplePools {
        credential: String,
        first_pool: String,
        second_pool: String,
    },
    #[error("pool {pool} provider {pool_provider} does not match candidate provider {candidate_provider}")]
    CrossProviderPool {
        pool: String,
        pool_provider: String,
        candidate_provider: String,
    },
    #[error("candidate has both credential and credential_pool bindings")]
    CandidateBindingConflict,
    #[error("alias {alias} has no candidates")]
    EmptyAlias { alias: String },
    #[error("strict alias {alias} candidate {model} has unknown context metadata")]
    UnknownContext { alias: String, model: String },
    #[error("strict alias {alias} candidate {model} has unknown context metadata")]
    UnverifiedContext { alias: String, model: String },
    #[error("strict alias {alias} candidate {model} has only {context} context tokens")]
    InsufficientContext {
        alias: String,
        model: String,
        context: usize,
    },
    #[error("provider {0} is referenced by another provider record")]
    ReferencedProvider(String),
    #[error("credential {0} is referenced by an alias")]
    ReferencedCredential(String),
    #[error("credential {credential} is referenced by pool {pool}")]
    ReferencedCredentialPool { credential: String, pool: String },
    #[error("credential pool {pool} is referenced by aliases: {aliases}")]
    ReferencedPool { pool: String, aliases: String },
}

#[derive(Debug, Clone)]
pub struct ProviderRegistry {
    providers: BTreeMap<ProviderId, Provider>,
    credentials: BTreeMap<CredentialId, Credential>,
    pools: BTreeMap<CredentialPoolId, CredentialPool>,
    models: BTreeMap<(ProviderId, String), ModelInfo>,
    aliases: BTreeMap<AliasId, ModelAlias>,
    active_alias: Option<AliasId>,
}

#[derive(Debug, Clone)]
pub struct ProviderSnapshot {
    pub providers: Arc<BTreeMap<ProviderId, Provider>>,
    pub credentials: Arc<BTreeMap<CredentialId, Credential>>,
    pub pools: Arc<BTreeMap<CredentialPoolId, CredentialPool>>,
    pub models: Arc<BTreeMap<(ProviderId, String), ModelInfo>>,
    pub aliases: Arc<BTreeMap<AliasId, ModelAlias>>,
    pub active_alias: Option<AliasId>,
}

/// One immutable generation of provider configuration and the mutable
/// process-local scheduler attached to that generation. Requests keep this
/// Arc until their response lease is released, so reload cannot invalidate an
/// in-flight reservation.
#[derive(Debug, Clone)]
pub struct ProviderRuntimeSnapshot {
    pub registry: Arc<ProviderSnapshot>,
    pub scheduler: Arc<CapacityScheduler>,
}

impl std::ops::Deref for ProviderRuntimeSnapshot {
    type Target = ProviderSnapshot;

    fn deref(&self) -> &Self::Target {
        &self.registry
    }
}

/// Atomic runtime boundary. A request takes one immutable snapshot; replacing
/// the handle never mutates a request already in flight.
#[derive(Debug, Clone)]
pub struct ProviderRuntimeHandle {
    current: Arc<RwLock<Arc<ProviderRuntimeSnapshot>>>,
}

impl ProviderRuntimeHandle {
    pub fn load(registry: &ProviderRegistry) -> Result<Self, RegistryError> {
        let snapshot = Arc::new(registry.compile_snapshot()?);
        let scheduler = CapacityScheduler::from_snapshot(&snapshot);
        Ok(Self {
            current: Arc::new(RwLock::new(Arc::new(ProviderRuntimeSnapshot {
                registry: snapshot,
                scheduler,
            }))),
        })
    }
    pub fn snapshot(&self) -> Arc<ProviderRuntimeSnapshot> {
        self.current
            .read()
            .expect("provider runtime lock poisoned")
            .clone()
    }
    pub fn replace(&self, registry: &ProviderRegistry) -> Result<(), RegistryError> {
        let snapshot = Arc::new(registry.compile_snapshot()?);
        let scheduler = CapacityScheduler::from_snapshot(&snapshot);
        let next = Arc::new(ProviderRuntimeSnapshot {
            registry: snapshot,
            scheduler,
        });
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
            pools: BTreeMap::new(),
            models: BTreeMap::new(),
            aliases: BTreeMap::new(),
            active_alias: None,
        }
    }
    pub fn from_snapshot(snapshot: &ProviderSnapshot) -> Self {
        Self {
            providers: (*snapshot.providers).clone(),
            credentials: (*snapshot.credentials).clone(),
            pools: (*snapshot.pools).clone(),
            models: (*snapshot.models).clone(),
            aliases: (*snapshot.aliases).clone(),
            active_alias: snapshot.active_alias.clone(),
        }
    }
    pub fn providers(&self) -> impl Iterator<Item = &Provider> {
        self.providers.values()
    }
    pub fn credentials(&self) -> impl Iterator<Item = &Credential> {
        self.credentials.values()
    }
    pub fn pools(&self) -> impl Iterator<Item = &CredentialPool> {
        self.pools.values()
    }
    pub fn pool(&self, id: impl AsRef<str>) -> Option<&CredentialPool> {
        self.pools.get(&CredentialPoolId::new(id.as_ref()).ok()?)
    }
    pub fn aliases(&self) -> impl Iterator<Item = &ModelAlias> {
        self.aliases.values()
    }
    pub fn active_alias(&self) -> Option<&AliasId> {
        self.active_alias.as_ref()
    }
    pub fn set_active_alias(&mut self, alias: Option<AliasId>) {
        self.active_alias = alias;
    }
    pub fn set_enabled(&mut self, id: impl AsRef<str>, enabled: bool) -> Result<(), RegistryError> {
        let provider = self
            .providers
            .get_mut(&ProviderId::from(id.as_ref()))
            .ok_or_else(|| RegistryError::UnknownProvider(id.as_ref().to_string()))?;
        provider.enabled = enabled;
        Ok(())
    }
    pub fn upsert_credential(&mut self, credential: Credential) -> Result<(), RegistryError> {
        if !self.providers.contains_key(&credential.provider_id) {
            return Err(RegistryError::UnknownProvider(
                credential.provider_id.to_string(),
            ));
        }
        self.credentials.insert(credential.id.clone(), credential);
        Ok(())
    }
    pub fn remove_credential(&mut self, id: impl AsRef<str>) -> Result<(), RegistryError> {
        let id = CredentialId::from(id.as_ref());
        if self.aliases.values().any(|alias| {
            alias
                .candidates
                .iter()
                .any(|candidate| candidate.credential_id.as_ref() == Some(&id))
        }) {
            return Err(RegistryError::ReferencedCredential(id.to_string()));
        }
        if let Some(pool) = self
            .pools
            .values()
            .find(|pool| pool.members.iter().any(|member| member.credential_id == id))
        {
            return Err(RegistryError::ReferencedCredentialPool {
                credential: id.to_string(),
                pool: pool.id.to_string(),
            });
        }
        self.credentials
            .remove(&id)
            .map(|_| ())
            .ok_or_else(|| RegistryError::UnknownCredential(id.to_string()))
    }
    pub fn remove_model(
        &mut self,
        provider: impl AsRef<str>,
        model: impl AsRef<str>,
    ) -> Result<(), RegistryError> {
        let provider_id = ProviderId::from(provider.as_ref());
        let model_id = model.as_ref().to_string();
        if self.aliases.values().any(|alias| {
            alias.candidates.iter().any(|candidate| {
                candidate.provider_id == provider_id && candidate.model_id == model_id
            })
        }) {
            return Err(RegistryError::ReferencedProvider(format!(
                "{}:{}",
                provider_id, model_id
            )));
        }
        self.models
            .remove(&(provider_id.clone(), model_id.clone()))
            .map(|_| ())
            .ok_or_else(|| RegistryError::UnknownModel {
                provider: provider_id.to_string(),
                model: model_id,
            })
    }
    pub fn remove_alias(&mut self, id: impl AsRef<str>) -> Result<(), RegistryError> {
        let id = AliasId::from(id.as_ref());
        self.aliases
            .remove(&id)
            .map(|_| ())
            .ok_or_else(|| RegistryError::UnknownModel {
                provider: "alias".to_string(),
                model: id.to_string(),
            })
    }

    pub fn upsert_pool(&mut self, pool: CredentialPool) -> Result<(), RegistryError> {
        self.validate_pool(&pool)?;
        self.pools.insert(pool.id.clone(), pool);
        Ok(())
    }

    pub fn remove_pool(&mut self, id: impl AsRef<str>) -> Result<(), RegistryError> {
        let id = CredentialPoolId::new(id.as_ref())
            .map_err(|_| RegistryError::UnknownPool(id.as_ref().to_string()))?;
        let aliases = self
            .aliases
            .values()
            .filter(|alias| {
                alias
                    .candidates
                    .iter()
                    .any(|candidate| candidate.credential_pool_id.as_ref() == Some(&id))
            })
            .map(|alias| alias.id.to_string())
            .collect::<Vec<_>>();
        if !aliases.is_empty() {
            return Err(RegistryError::ReferencedPool {
                pool: id.to_string(),
                aliases: aliases.join(", "),
            });
        }
        self.pools
            .remove(&id)
            .map(|_| ())
            .ok_or_else(|| RegistryError::UnknownPool(id.to_string()))
    }
    pub fn remove_provider(&mut self, id: impl AsRef<str>) -> Result<(), RegistryError> {
        let id = ProviderId::from(id.as_ref());
        if self
            .credentials
            .values()
            .any(|credential| credential.provider_id == id)
            || self.models.keys().any(|(provider, _)| provider == &id)
            || self.aliases.values().any(|alias| {
                alias
                    .candidates
                    .iter()
                    .any(|candidate| candidate.provider_id == id)
            })
        {
            return Err(RegistryError::ReferencedProvider(id.to_string()));
        }
        self.providers
            .remove(&id)
            .map(|_| ())
            .ok_or_else(|| RegistryError::UnknownProvider(id.to_string()))
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
    pub fn upsert_alias(&mut self, alias: ModelAlias) -> Result<(), RegistryError> {
        self.aliases.remove(&alias.id);
        self.validate_alias(&alias)?;
        self.aliases.insert(alias.id.clone(), alias);
        Ok(())
    }
    pub fn compile_snapshot(&self) -> Result<ProviderSnapshot, RegistryError> {
        if let Some(active_alias) = self.active_alias.as_ref() {
            if !self.aliases.contains_key(active_alias) {
                return Err(RegistryError::UnknownModel {
                    provider: "alias".to_string(),
                    model: active_alias.to_string(),
                });
            }
        }
        for alias in self.aliases.values() {
            self.validate_alias(alias)?;
        }
        for pool in self.pools.values() {
            self.validate_pool(pool)?;
        }
        let mut shared_scopes = BTreeMap::<String, (Option<u32>, Option<u64>)>::new();
        for pool in self.pools.values() {
            for member in &pool.members {
                let budget = (
                    member.requests_per_minute.map(|value| value.get()),
                    member.tokens_per_minute.map(|value| value.get()),
                );
                if let Some(previous) = shared_scopes.insert(member.quota_scope.clone(), budget) {
                    if previous != budget {
                        return Err(RegistryError::InvalidPool {
                            pool: pool.id.to_string(),
                            reason: format!(
                                "members sharing quota scope {} across pools must use identical RPM/TPM limits",
                                member.quota_scope
                            ),
                        });
                    }
                }
            }
        }
        Ok(ProviderSnapshot {
            providers: Arc::new(self.providers.clone()),
            credentials: Arc::new(self.credentials.clone()),
            pools: Arc::new(self.pools.clone()),
            models: Arc::new(self.models.clone()),
            aliases: Arc::new(self.aliases.clone()),
            active_alias: self.active_alias.clone(),
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
        let mut targets = Vec::new();
        for (candidate_index, (_, candidate)) in candidates.into_iter().enumerate() {
            let info = self
                .models
                .get(&(candidate.provider_id.clone(), candidate.model_id.clone()))
                .ok_or_else(|| RegistryError::UnknownModel {
                    provider: candidate.provider_id.to_string(),
                    model: candidate.model_id.clone(),
                })?;
            let credential_ids = match candidate
                .binding()
                .map_err(|_| RegistryError::CandidateBindingConflict)?
            {
                CredentialBinding::Anonymous => vec![None],
                CredentialBinding::Direct(id) => vec![Some(id)],
                CredentialBinding::Pool(pool_id) => self
                    .pools
                    .get(&pool_id)
                    .ok_or_else(|| RegistryError::UnknownPool(pool_id.to_string()))?
                    .members
                    .iter()
                    .map(|member| Some(member.credential_id.clone()))
                    .collect(),
            };
            for credential_id in credential_ids {
                targets.push(AttemptTarget {
                    alias_id: alias.id.clone(),
                    client_model: alias.client_model.clone(),
                    provider_id: info.provider_id.clone(),
                    model_id: info.model_id.clone(),
                    wire_model_id: info.wire_model_id.clone(),
                    auth_scheme: credential_id
                        .as_ref()
                        .and_then(|id| {
                            self.credentials
                                .get(id)
                                .map(|credential| credential.auth_scheme)
                        })
                        .unwrap_or(AuthScheme::None),
                    credential_id,
                    context_window: info.context_window.unwrap_or_default(),
                    candidate_index,
                });
            }
        }
        Ok(targets)
    }

    pub fn resolve_routes(&self, id: impl AsRef<str>) -> Result<Vec<RouteTarget>, RegistryError> {
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
                let binding = candidate
                    .binding()
                    .map_err(|_| RegistryError::CandidateBindingConflict)?;
                Ok(RouteTarget {
                    alias_id: alias.id.clone(),
                    client_model: alias.client_model.clone(),
                    provider_id: info.provider_id.clone(),
                    model_id: info.model_id.clone(),
                    wire_model_id: info.wire_model_id.clone(),
                    binding,
                    context_window: info.context_window.unwrap_or_default(),
                    max_output_tokens: info.max_output_tokens,
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
            id: AliasId::from("legacy-default"),
            client_model: "claude-sonnet-5".to_string(),
            context_window: 128_000,
            strict_context: false,
            candidates: vec![ModelCandidate {
                provider_id,
                model_id: model,
                credential_id,
                credential_pool_id: None,
                priority: 0,
            }],
        });
        out.set_active_alias(Some(AliasId::from("legacy-default")));
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
            let binding = candidate
                .binding()
                .map_err(|_| RegistryError::CandidateBindingConflict)?;
            match binding {
                CredentialBinding::Anonymous => {}
                CredentialBinding::Direct(credential) => {
                    let credential_obj = self
                        .credentials
                        .get(&credential)
                        .ok_or_else(|| RegistryError::UnknownCredential(credential.to_string()))?;
                    if credential_obj.provider_id != candidate.provider_id {
                        return Err(RegistryError::CrossProviderCredential {
                            credential: credential.to_string(),
                            provider: candidate.provider_id.to_string(),
                        });
                    }
                }
                CredentialBinding::Pool(pool) => {
                    let pool_obj = self
                        .pools
                        .get(&pool)
                        .ok_or_else(|| RegistryError::UnknownPool(pool.to_string()))?;
                    if pool_obj.provider_id != candidate.provider_id {
                        return Err(RegistryError::CrossProviderPool {
                            pool: pool.to_string(),
                            pool_provider: pool_obj.provider_id.to_string(),
                            candidate_provider: candidate.provider_id.to_string(),
                        });
                    }
                }
            }
            if alias.strict_context {
                match info.context_window {
                    Some(value) if value >= alias.context_window && info.verified_context => {}
                    Some(_) if !info.verified_context => {
                        return Err(RegistryError::UnverifiedContext {
                            alias: alias.id.to_string(),
                            model: candidate.model_id.clone(),
                        })
                    }
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

    fn validate_pool(&self, pool: &CredentialPool) -> Result<(), RegistryError> {
        if !self.providers.contains_key(&pool.provider_id) {
            return Err(RegistryError::UnknownProvider(pool.provider_id.to_string()));
        }
        if pool.members.is_empty() {
            return Err(RegistryError::InvalidPool {
                pool: pool.id.to_string(),
                reason: "pool must contain at least one member".to_string(),
            });
        }
        let mut seen = BTreeMap::<CredentialId, ()>::new();
        let mut scopes = BTreeMap::<String, (Option<u32>, Option<u64>)>::new();
        for member in &pool.members {
            if seen.insert(member.credential_id.clone(), ()).is_some() {
                return Err(RegistryError::InvalidPoolMember {
                    pool: pool.id.to_string(),
                    credential: member.credential_id.to_string(),
                    reason: "credential is duplicated in the pool".to_string(),
                });
            }
            let credential = self.credentials.get(&member.credential_id).ok_or_else(|| {
                RegistryError::UnknownCredential(member.credential_id.to_string())
            })?;
            if credential.provider_id != pool.provider_id {
                return Err(RegistryError::CrossProviderCredential {
                    credential: member.credential_id.to_string(),
                    provider: pool.provider_id.to_string(),
                });
            }
            if member.quota_scope.trim().is_empty() {
                return Err(RegistryError::InvalidPoolMember {
                    pool: pool.id.to_string(),
                    credential: member.credential_id.to_string(),
                    reason: "quota scope must not be empty".to_string(),
                });
            }
            if member.max_in_flight.get() > 1024
                || member
                    .requests_per_minute
                    .is_some_and(|value| value.get() > 10_000_000)
                || member
                    .tokens_per_minute
                    .is_some_and(|value| value.get() > 10_000_000_000)
                || member.weight.get() > 1000
                || (pool.strategy != PoolStrategy::WeightedRoundRobin && member.weight.get() != 1)
            {
                return Err(RegistryError::InvalidPoolMember {
                    pool: pool.id.to_string(),
                    credential: member.credential_id.to_string(),
                    reason: "member limits or strategy weight are out of range".to_string(),
                });
            }
            let budget = (
                member.requests_per_minute.map(|value| value.get()),
                member.tokens_per_minute.map(|value| value.get()),
            );
            if let Some(previous) = scopes.insert(member.quota_scope.clone(), budget) {
                if previous != budget {
                    return Err(RegistryError::InvalidPool {
                        pool: pool.id.to_string(),
                        reason: format!(
                            "members sharing quota scope {} must use identical RPM/TPM limits",
                            member.quota_scope
                        ),
                    });
                }
            }
        }
        for (other_id, other) in &self.pools {
            if other_id == &pool.id {
                continue;
            }
            for member in &pool.members {
                if other
                    .members
                    .iter()
                    .any(|candidate| candidate.credential_id == member.credential_id)
                {
                    return Err(RegistryError::CredentialInMultiplePools {
                        credential: member.credential_id.to_string(),
                        first_pool: other_id.to_string(),
                        second_pool: pool.id.to_string(),
                    });
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
                    credential_pool_id: None,
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
                    credential_pool_id: None,
                    priority: 0,
                }],
            ))
            .unwrap_err();
        assert!(matches!(err, RegistryError::CrossProviderCredential { .. }));
    }

    #[test]
    fn resolve_alias_materializes_pool_members_for_legacy_callers() {
        let mut r = ProviderRegistry::new();
        r.register_provider(provider("bai")).unwrap();
        r.insert_model(model("bai", "model-x", Some(1_000_000)));
        for id in ["key-a", "key-b"] {
            r.register_credential(Credential {
                id: id.into(),
                provider_id: "bai".into(),
                source: SecretSource::Managed { id: id.into() },
                auth_scheme: AuthScheme::Bearer,
            })
            .unwrap();
        }
        r.upsert_pool(
            CredentialPool::new(
                "pool",
                "bai",
                PoolStrategy::RoundRobin,
                vec![
                    CredentialPoolMember::new("key-a", "account-a", 1, None, None, 1).unwrap(),
                    CredentialPoolMember::new("key-b", "account-b", 1, None, None, 1).unwrap(),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        r.register_alias(ModelAlias::one_million(
            "alias",
            vec![ModelCandidate {
                provider_id: "bai".into(),
                model_id: "model-x".into(),
                credential_id: None,
                credential_pool_id: Some("pool".into()),
                priority: 0,
            }],
        ))
        .unwrap();
        let targets = r.resolve_alias("alias").unwrap();
        assert_eq!(
            targets
                .iter()
                .map(|target| target.credential_id.as_ref().unwrap().as_ref())
                .collect::<Vec<_>>(),
            ["key-a", "key-b"]
        );
    }
}
