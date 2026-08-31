//! API-key registry: persistence, CRUD, matching, and staged commits.

use super::types::*;
use crate::config::BridgeConfig;
use crate::infrastructure::file_store::FileStore;
use crate::management::auth::token_eq;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug)]
pub struct ApiKeyRegistry {
    pub(crate) path: PathBuf,
    pub(crate) file: ApiKeyRegistryFile,
    pub(crate) runtimes: HashMap<String, Arc<ApiKeyRuntime>>,
}

impl ApiKeyRegistry {
    pub fn load(config: &BridgeConfig, store: &dyn FileStore) -> Result<Self, ApiKeyError> {
        let path = registry_path(&config.management.config_path);
        let mut file = match store.read(&path) {
            Ok(bytes) => serde_json::from_slice::<ApiKeyRegistryFile>(&bytes)
                .map_err(ApiKeyError::RegistryParse)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => ApiKeyRegistryFile::default(),
            Err(error) => return Err(ApiKeyError::Read(error)),
        };
        if file.schema_version != REGISTRY_SCHEMA_VERSION {
            file.schema_version = REGISTRY_SCHEMA_VERSION;
        }
        for record in &mut file.keys {
            record.normalize();
        }
        super::legacy::import_legacy_records(config, &mut file);
        let runtimes = file
            .keys
            .iter()
            .map(|record| {
                (
                    record.id.clone(),
                    Arc::new(ApiKeyRuntime::new(&record.policy, None)),
                )
            })
            .collect();
        Ok(Self {
            path,
            file,
            runtimes,
        })
    }

    pub fn load_or_default(config: &BridgeConfig, store: &dyn FileStore) -> Self {
        match Self::load(config, store) {
            Ok(registry) => registry,
            Err(error) => {
                tracing::warn!(%error, "failed to load API-key registry; using legacy in-memory keys");
                let mut file = ApiKeyRegistryFile::default();
                super::legacy::import_legacy_records(config, &mut file);
                let runtimes = file
                    .keys
                    .iter()
                    .map(|record| {
                        (
                            record.id.clone(),
                            Arc::new(ApiKeyRuntime::new(&record.policy, None)),
                        )
                    })
                    .collect();
                Self {
                    path: registry_path(&config.management.config_path),
                    file,
                    runtimes,
                }
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn configured(&self) -> bool {
        !self.file.keys.is_empty() || !self.file.suppressed_legacy_hashes.is_empty()
    }

    pub fn persist(&self, store: &dyn FileStore) -> Result<(), ApiKeyError> {
        let bytes =
            serde_json::to_vec_pretty(&self.file).map_err(ApiKeyError::RegistrySerialize)?;
        store
            .atomic_write(&self.path, &bytes, true)
            .map_err(ApiKeyError::Write)
    }

    /// Produce an isolated staging copy of the registry.
    ///
    /// Mutating dashboard handlers apply their change to the staging copy,
    /// persist it atomically, and only then [`Self::commit`] it into shared
    /// memory. If persistence fails, the live registry is never touched, so an
    /// error response can never leave memory and disk disagreeing.
    pub fn stage(&self) -> Self {
        Self {
            path: self.path.clone(),
            file: self.file.clone(),
            runtimes: self.runtimes.clone(),
        }
    }

    /// Replace shared in-memory state with a staging copy that has already
    /// been persisted successfully.
    ///
    /// Runtime usage counters live behind `Arc`s, so entries the staged
    /// mutation did not replace keep counting seamlessly across the commit.
    pub fn commit(&mut self, staged: Self) {
        self.file = staged.file;
        self.runtimes = staged.runtimes;
    }

    pub fn list(&self) -> Vec<ApiKeyView> {
        let now = unix_timestamp();
        self.file
            .keys
            .iter()
            .enumerate()
            .filter(|(_, record)| record.status != ApiKeyStatus::Revoked)
            .map(|(index, record)| self.view_for(record, index, now))
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<ApiKeyView> {
        let now = unix_timestamp();
        self.file
            .keys
            .iter()
            .enumerate()
            .find(|(_, record)| record.id == id)
            .map(|(index, record)| self.view_for(record, index, now))
    }

    pub fn create(
        &mut self,
        name: String,
        description: Option<String>,
        environment: String,
        expires_at: Option<u64>,
        mut policy: ApiKeyPolicy,
        secret_bytes: usize,
    ) -> Result<(ApiKeyView, String), ApiKeyError> {
        let name = name.trim().to_string();
        if name.is_empty() || name.chars().count() > 80 {
            return Err(ApiKeyError::InvalidName);
        }
        if !(16..=64).contains(&secret_bytes) {
            return Err(ApiKeyError::InvalidSize);
        }
        policy.normalize();
        let id = self.unique_id()?;
        let secret = crate::infrastructure::random::secure_random_hex(secret_bytes)
            .map_err(|error| ApiKeyError::Random(error.to_string()))?;
        let token = format!("{DEFAULT_KEY_PREFIX}{id}.{secret}");
        let now = unix_timestamp();
        let record = ApiKeyRecord {
            id: id.clone(),
            name,
            description: clean_optional(description),
            fingerprint: fingerprint(&token),
            secret_hash: hash_token(&token),
            status: ApiKeyStatus::Active,
            environment: normalize_environment(&environment),
            created_at: now,
            expires_at,
            source: ApiKeySource::Managed,
            policy,
        };
        self.file.keys.push(record);
        let record = self.file.keys.last().expect("just inserted");
        self.runtimes
            .insert(id, Arc::new(ApiKeyRuntime::new(&record.policy, None)));
        let view = self.view_for(record, self.file.keys.len() - 1, now);
        Ok((view, token))
    }

    pub fn update(&mut self, id: &str, update: ApiKeyUpdate) -> Result<ApiKeyView, ApiKeyError> {
        let index = self
            .file
            .keys
            .iter()
            .position(|record| record.id == id)
            .ok_or(ApiKeyError::NotFound)?;
        let previous_usage = self.runtimes.get(id).map(|runtime| runtime.snapshot());
        if self.file.keys[index].status == ApiKeyStatus::Revoked {
            return Err(ApiKeyError::RevokedImmutable);
        }
        let record = &mut self.file.keys[index];
        if let Some(name) = update.name {
            let name = name.trim().to_string();
            if name.is_empty() || name.chars().count() > 80 {
                return Err(ApiKeyError::InvalidName);
            }
            record.name = name;
        }
        if let Some(description) = update.description {
            record.description = clean_optional(description);
        }
        if let Some(environment) = update.environment {
            record.environment = normalize_environment(&environment);
        }
        if let Some(expires_at) = update.expires_at {
            record.expires_at = expires_at;
        }
        if let Some(status) = update.status {
            record.status = status;
        }
        if let Some(mut policy) = update.policy {
            policy.normalize();
            record.policy = policy;
        }
        record.normalize();
        let runtime_policy = record.policy.clone();
        self.runtimes.insert(
            id.to_string(),
            Arc::new(ApiKeyRuntime::new(&runtime_policy, previous_usage)),
        );
        let record = self.file.keys[index].clone();
        Ok(self.view_for(&record, index, unix_timestamp()))
    }

    pub fn rotate(
        &mut self,
        id: &str,
        secret_bytes: usize,
    ) -> Result<(ApiKeyView, String), ApiKeyError> {
        if !(16..=64).contains(&secret_bytes) {
            return Err(ApiKeyError::InvalidSize);
        }
        let index = self
            .file
            .keys
            .iter()
            .position(|record| record.id == id)
            .ok_or(ApiKeyError::NotFound)?;
        // Preserve usage counters exactly like `update` does: rotating a
        // secret must never reset a daily quota or rate-limit window.
        let previous_usage = self.runtimes.get(id).map(|runtime| runtime.snapshot());
        let secret = crate::infrastructure::random::secure_random_hex(secret_bytes)
            .map_err(|error| ApiKeyError::Random(error.to_string()))?;
        let token = format!("{DEFAULT_KEY_PREFIX}{id}.{secret}");
        let record = &mut self.file.keys[index];
        if record.source == ApiKeySource::Legacy {
            self.file
                .suppressed_legacy_hashes
                .push(record.secret_hash.clone());
            self.file.suppressed_legacy_hashes.sort();
            self.file.suppressed_legacy_hashes.dedup();
        }
        record.secret_hash = hash_token(&token);
        record.fingerprint = fingerprint(&token);
        record.status = ApiKeyStatus::Active;
        record.source = ApiKeySource::Managed;
        let runtime_policy = record.policy.clone();
        self.runtimes.insert(
            id.to_string(),
            Arc::new(ApiKeyRuntime::new(&runtime_policy, previous_usage)),
        );
        let record = self.file.keys[index].clone();
        Ok((self.view_for(&record, index, unix_timestamp()), token))
    }

    pub fn revoke(&mut self, id: &str) -> Result<ApiKeyView, ApiKeyError> {
        let index = self
            .file
            .keys
            .iter()
            .position(|record| record.id == id)
            .ok_or(ApiKeyError::NotFound)?;
        let record = &mut self.file.keys[index];
        if record.source == ApiKeySource::Legacy {
            self.file
                .suppressed_legacy_hashes
                .push(record.secret_hash.clone());
            self.file.suppressed_legacy_hashes.sort();
            self.file.suppressed_legacy_hashes.dedup();
        }
        record.status = ApiKeyStatus::Revoked;
        let record = self.file.keys[index].clone();
        Ok(self.view_for(&record, index, unix_timestamp()))
    }

    pub fn match_secret(
        &self,
        token: &str,
        path: &str,
    ) -> Result<ApiKeyAuthMatch, ApiKeyAuthError> {
        let digest = hash_token(token);
        let (record, runtime) =
            self.file
                .keys
                .iter()
                .find_map(|record| {
                    token_eq(record.secret_hash.as_bytes(), digest.as_bytes()).then(|| {
                        (
                            record.clone(),
                            self.runtimes.get(&record.id).cloned().unwrap_or_else(|| {
                                Arc::new(ApiKeyRuntime::new(&record.policy, None))
                            }),
                        )
                    })
                })
                .ok_or(ApiKeyAuthError::Invalid)?;
        match record.status {
            ApiKeyStatus::Disabled => return Err(ApiKeyAuthError::Disabled),
            ApiKeyStatus::Revoked => return Err(ApiKeyAuthError::Revoked),
            ApiKeyStatus::Active => {}
        }
        if record.expired(unix_timestamp()) {
            return Err(ApiKeyAuthError::Expired);
        }
        if !record.policy.endpoint_allowed(path) {
            return Err(ApiKeyAuthError::EndpointDenied(path.to_string()));
        }
        Ok(ApiKeyAuthMatch { record, runtime })
    }

    pub fn verify(&self, token: &str) -> Result<ApiKeyView, ApiKeyAuthError> {
        let digest = hash_token(token);
        let now = unix_timestamp();
        self.file
            .keys
            .iter()
            .enumerate()
            .find(|(_, record)| token_eq(record.secret_hash.as_bytes(), digest.as_bytes()))
            .map(|(index, record)| self.view_for(record, index, now))
            .ok_or(ApiKeyAuthError::Invalid)
    }

    fn unique_id(&self) -> Result<String, ApiKeyError> {
        for _ in 0..10 {
            let random = crate::infrastructure::random::secure_random_hex(8)
                .map_err(|error| ApiKeyError::Random(error.to_string()))?;
            let id = format!("key_{random}");
            if !self.file.keys.iter().any(|record| record.id == id) {
                return Ok(id);
            }
        }
        Err(ApiKeyError::Random(
            "failed to allocate a unique key id".to_string(),
        ))
    }

    fn view_for(&self, record: &ApiKeyRecord, index: usize, now: u64) -> ApiKeyView {
        let usage = self
            .runtimes
            .get(&record.id)
            .map(|runtime| runtime.view())
            .unwrap_or_default();
        ApiKeyView {
            id: record.id.clone(),
            name: record.name.clone(),
            description: record.description.clone(),
            fingerprint: record.fingerprint.clone(),
            status: record.status,
            environment: record.environment.clone(),
            created_at: record.created_at,
            expires_at: record.expires_at,
            expired: record.expired(now),
            source: record.source,
            policy: record.policy.clone(),
            usage,
            index,
            length: record.fingerprint.chars().count(),
            active: record.status == ApiKeyStatus::Active && !record.expired(now),
        }
    }
}

/// Compute the registry JSON file path adjacent to the TOML config.
pub fn registry_path(config_path: &Path) -> PathBuf {
    let name = config_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("opencode2api");
    config_path.with_file_name(format!("{name}.api-keys.json"))
}
