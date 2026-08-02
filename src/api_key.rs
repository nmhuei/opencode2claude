//! API-key lifecycle, policy, hot-reload registry, and legacy TOML compatibility.
//!
//! Managed keys are persisted in a sidecar JSON file next to the main TOML
//! configuration. Only SHA-256 digests are stored. A newly generated secret is
//! returned exactly once to the caller and never written to disk.

use crate::config::{BridgeConfig, TomlConfig};
use crate::infrastructure::file_store::{AtomicFileStore, FileStore};
use crate::management::auth::token_eq;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use toml_edit::{value, Array, DocumentMut, Item, Value};

const REGISTRY_SCHEMA_VERSION: u32 = 1;
const DEFAULT_KEY_PREFIX: &str = "sk-oc2-";

#[derive(Debug, thiserror::Error)]
pub enum ApiKeyError {
    #[error("API-key size must be between 16 and 64 bytes")]
    InvalidSize,
    #[error("API-key count must be between 1 and 20")]
    InvalidCount,
    #[error("at least one valid API-key index must be selected")]
    InvalidSelection,
    #[error("API-key name must contain between 1 and 80 characters")]
    InvalidName,
    #[error("API-key record was not found")]
    NotFound,
    #[error("revoked API keys are immutable")]
    RevokedImmutable,
    #[error("failed to generate secure random bytes: {0}")]
    Random(String),
    #[error("failed to read API-key data: {0}")]
    Read(#[source] io::Error),
    #[error("invalid API-key registry: {0}")]
    RegistryParse(#[source] serde_json::Error),
    #[error("invalid TOML configuration: {0}")]
    Parse(#[from] toml_edit::TomlError),
    #[error("updated configuration is invalid: {0}")]
    Validation(#[source] toml::de::Error),
    #[error("failed to write API-key data atomically: {0}")]
    Write(#[source] io::Error),
    #[error("failed to serialize API-key registry: {0}")]
    RegistrySerialize(#[source] serde_json::Error),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyStatus {
    #[default]
    Active,
    Disabled,
    Revoked,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeySource {
    #[default]
    Managed,
    Legacy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningMode {
    #[default]
    Inherit,
    Disabled,
    Enabled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LimitAction {
    #[default]
    Reject,
    Clamp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiKeyPermissions {
    #[serde(default = "default_true")]
    pub anthropic_messages: bool,
    #[serde(default = "default_true")]
    pub openai_chat: bool,
    #[serde(default = "default_true")]
    pub list_models: bool,
    #[serde(default = "default_true")]
    pub count_tokens: bool,
    #[serde(default = "default_true")]
    pub streaming: bool,
    #[serde(default = "default_true")]
    pub tools: bool,
    #[serde(default = "default_true")]
    pub web_search: bool,
    #[serde(default)]
    pub shell: bool,
}

impl Default for ApiKeyPermissions {
    fn default() -> Self {
        Self {
            anthropic_messages: true,
            openai_chat: true,
            list_models: true,
            count_tokens: true,
            streaming: true,
            tools: true,
            web_search: true,
            shell: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiKeyPolicy {
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub allowed_models: Vec<String>,
    #[serde(default = "default_true")]
    pub allow_model_override: bool,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub max_reasoning_tokens: Option<u32>,
    #[serde(default)]
    pub reasoning_mode: ReasoningMode,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub limit_action: LimitAction,
    #[serde(default)]
    pub max_concurrent_requests: Option<usize>,
    #[serde(default)]
    pub requests_per_minute: Option<u32>,
    #[serde(default)]
    pub daily_request_quota: Option<u64>,
    #[serde(default)]
    pub permissions: ApiKeyPermissions,
}

impl Default for ApiKeyPolicy {
    fn default() -> Self {
        Self {
            default_model: None,
            allowed_models: Vec::new(),
            allow_model_override: true,
            max_output_tokens: None,
            max_reasoning_tokens: None,
            reasoning_mode: ReasoningMode::Inherit,
            reasoning_effort: None,
            limit_action: LimitAction::Reject,
            max_concurrent_requests: None,
            requests_per_minute: None,
            daily_request_quota: None,
            permissions: ApiKeyPermissions::default(),
        }
    }
}

impl ApiKeyPolicy {
    pub fn normalize(&mut self) {
        self.default_model = clean_optional(self.default_model.take());
        self.reasoning_effort =
            clean_optional(self.reasoning_effort.take()).map(|value| value.to_ascii_lowercase());
        self.allowed_models = self
            .allowed_models
            .drain(..)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect();
        self.allowed_models.sort();
        self.allowed_models.dedup();
        self.max_concurrent_requests = nonzero(self.max_concurrent_requests);
        self.requests_per_minute = nonzero(self.requests_per_minute);
        self.daily_request_quota = nonzero(self.daily_request_quota);
        self.max_output_tokens = nonzero(self.max_output_tokens);
        self.max_reasoning_tokens = nonzero(self.max_reasoning_tokens);
    }

    pub fn endpoint_allowed(&self, path: &str) -> bool {
        match path {
            "/v1/messages" => self.permissions.anthropic_messages,
            "/v1/messages/count_tokens" => self.permissions.count_tokens,
            "/v1/chat/completions" => self.permissions.openai_chat,
            "/v1/models" => self.permissions.list_models,
            _ => true,
        }
    }

    pub fn resolve_model(
        &self,
        requested: Option<&str>,
        global: Option<&str>,
        fallback: &str,
    ) -> Result<String, ApiKeyPolicyError> {
        let requested = requested.map(str::trim).filter(|value| !value.is_empty());
        let selected = if self.allow_model_override {
            requested
                .or(self.default_model.as_deref())
                .or(global)
                .unwrap_or(fallback)
        } else {
            self.default_model.as_deref().or(global).unwrap_or(fallback)
        };

        if !self.allowed_models.is_empty()
            && !self
                .allowed_models
                .iter()
                .any(|allowed| allowed == selected)
        {
            return Err(ApiKeyPolicyError::ModelNotAllowed(selected.to_string()));
        }
        Ok(selected.to_string())
    }

    pub fn enforce_output_tokens(
        &self,
        requested: Option<u32>,
    ) -> Result<Option<u32>, ApiKeyPolicyError> {
        let Some(limit) = self.max_output_tokens else {
            return Ok(requested);
        };
        match requested {
            Some(value) if value > limit && self.limit_action == LimitAction::Reject => {
                Err(ApiKeyPolicyError::OutputLimitExceeded {
                    requested: value,
                    limit,
                })
            }
            Some(value) => Ok(Some(value.min(limit))),
            None => Ok(Some(limit)),
        }
    }

    pub fn enforce_reasoning_tokens(
        &self,
        requested: Option<u32>,
    ) -> Result<Option<u32>, ApiKeyPolicyError> {
        let Some(limit) = self.max_reasoning_tokens else {
            return Ok(requested);
        };
        match requested {
            Some(value) if value > limit && self.limit_action == LimitAction::Reject => {
                Err(ApiKeyPolicyError::ReasoningLimitExceeded {
                    requested: value,
                    limit,
                })
            }
            Some(value) => Ok(Some(value.min(limit))),
            None => Ok(Some(limit)),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiKeyPolicyError {
    #[error("model '{0}' is not allowed for this API key")]
    ModelNotAllowed(String),
    #[error("requested max_tokens {requested} exceeds this API key limit of {limit}")]
    OutputLimitExceeded { requested: u32, limit: u32 },
    #[error("requested reasoning budget {requested} exceeds this API key limit of {limit}")]
    ReasoningLimitExceeded { requested: u32, limit: u32 },
    #[error("streaming is disabled for this API key")]
    StreamingDisabled,
    #[error("tool use is disabled for this API key")]
    ToolsDisabled,
    #[error("web search is disabled for this API key")]
    WebSearchDisabled,
    #[error("shell execution is disabled for this API key")]
    ShellDisabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiKeyRecord {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub fingerprint: String,
    pub secret_hash: String,
    #[serde(default)]
    pub status: ApiKeyStatus,
    #[serde(default = "default_environment")]
    pub environment: String,
    pub created_at: u64,
    #[serde(default)]
    pub expires_at: Option<u64>,
    #[serde(default)]
    pub source: ApiKeySource,
    #[serde(default)]
    pub policy: ApiKeyPolicy,
}

impl ApiKeyRecord {
    fn normalize(&mut self) {
        self.name = self.name.trim().to_string();
        self.description = clean_optional(self.description.take());
        self.environment = normalize_environment(&self.environment);
        self.policy.normalize();
    }

    pub fn expired(&self, now: u64) -> bool {
        self.expires_at.is_some_and(|expires| expires <= now)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ApiKeyUpdate {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub environment: Option<String>,
    pub expires_at: Option<Option<u64>>,
    pub status: Option<ApiKeyStatus>,
    pub policy: Option<ApiKeyPolicy>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ApiKeyUsageView {
    pub requests: u64,
    pub rejected: u64,
    pub last_used_at: Option<u64>,
    pub in_flight: usize,
    pub minute_requests: u32,
    pub daily_requests: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiKeyView {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub fingerprint: String,
    pub status: ApiKeyStatus,
    pub environment: String,
    pub created_at: u64,
    pub expires_at: Option<u64>,
    pub expired: bool,
    pub source: ApiKeySource,
    pub policy: ApiKeyPolicy,
    pub usage: ApiKeyUsageView,
    // Compatibility fields for older dashboard clients.
    pub index: usize,
    pub length: usize,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub struct AuthenticatedClient {
    pub key_id: String,
    pub name: String,
    pub environment: String,
    pub policy: ApiKeyPolicy,
}

#[derive(Debug)]
pub struct ApiKeyAdmission {
    pub client: AuthenticatedClient,
    #[allow(dead_code)]
    permit: Option<OwnedSemaphorePermit>,
}

impl ApiKeyAdmission {
    /// Create an admission for the bridge-owned Claude Code integration key.
    ///
    /// This credential is intentionally outside the dashboard-managed API-key
    /// registry, so creating, rotating, disabling, or revoking application keys
    /// cannot interrupt the local Claude Code connection.
    pub(crate) fn claude_code(policy: ApiKeyPolicy) -> Self {
        Self {
            client: AuthenticatedClient {
                key_id: "system_claude_code".to_string(),
                name: "Claude Code".to_string(),
                environment: "local".to_string(),
                policy,
            },
            permit: None,
        }
    }
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ApiKeyAuthError {
    #[error("invalid API key")]
    Invalid,
    #[error("API key is disabled")]
    Disabled,
    #[error("API key has been revoked")]
    Revoked,
    #[error("API key has expired")]
    Expired,
    #[error("this API key is not allowed to access {0}")]
    EndpointDenied(String),
    #[error("API key concurrent request limit reached")]
    ConcurrentLimit,
    #[error("API key requests-per-minute limit reached")]
    RequestsPerMinute,
    #[error("API key daily request quota reached")]
    DailyQuota,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApiKeyRegistryFile {
    #[serde(default = "registry_schema_version")]
    schema_version: u32,
    #[serde(default)]
    keys: Vec<ApiKeyRecord>,
    #[serde(default)]
    suppressed_legacy_hashes: Vec<String>,
}

impl Default for ApiKeyRegistryFile {
    fn default() -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA_VERSION,
            keys: Vec::new(),
            suppressed_legacy_hashes: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct ApiKeyRegistry {
    path: PathBuf,
    file: ApiKeyRegistryFile,
    runtimes: HashMap<String, Arc<ApiKeyRuntime>>,
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
        import_legacy_records(config, &mut file);
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
                import_legacy_records(config, &mut file);
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
            Arc::new(ApiKeyRuntime::new(&runtime_policy, None)),
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

#[derive(Debug, Clone)]
pub struct ApiKeyAuthMatch {
    record: ApiKeyRecord,
    runtime: Arc<ApiKeyRuntime>,
}

impl ApiKeyAuthMatch {
    pub async fn admit(self) -> Result<ApiKeyAdmission, ApiKeyAuthError> {
        let permit = self.runtime.admit(&self.record.policy).await?;
        Ok(ApiKeyAdmission {
            client: AuthenticatedClient {
                key_id: self.record.id,
                name: self.record.name,
                environment: self.record.environment,
                policy: self.record.policy,
            },
            permit,
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct RuntimeSnapshot {
    requests: u64,
    rejected: u64,
    last_used_at: u64,
    minute_started: u64,
    minute_requests: u32,
    day_number: u64,
    daily_requests: u64,
}

#[derive(Debug, Default)]
struct RateWindow {
    minute_started: u64,
    minute_requests: u32,
    day_number: u64,
    daily_requests: u64,
}

#[derive(Debug)]
struct ApiKeyRuntime {
    semaphore: Option<Arc<Semaphore>>,
    max_concurrent: Option<usize>,
    requests: AtomicU64,
    rejected: AtomicU64,
    last_used_at: AtomicU64,
    window: Mutex<RateWindow>,
}

impl ApiKeyRuntime {
    fn new(policy: &ApiKeyPolicy, snapshot: Option<RuntimeSnapshot>) -> Self {
        let snapshot = snapshot.unwrap_or_default();
        Self {
            semaphore: policy
                .max_concurrent_requests
                .filter(|value| *value > 0)
                .map(|value| Arc::new(Semaphore::new(value))),
            max_concurrent: policy.max_concurrent_requests,
            requests: AtomicU64::new(snapshot.requests),
            rejected: AtomicU64::new(snapshot.rejected),
            last_used_at: AtomicU64::new(snapshot.last_used_at),
            window: Mutex::new(RateWindow {
                minute_started: snapshot.minute_started,
                minute_requests: snapshot.minute_requests,
                day_number: snapshot.day_number,
                daily_requests: snapshot.daily_requests,
            }),
        }
    }

    async fn admit(
        &self,
        policy: &ApiKeyPolicy,
    ) -> Result<Option<OwnedSemaphorePermit>, ApiKeyAuthError> {
        let permit = match &self.semaphore {
            Some(semaphore) => Some(semaphore.clone().try_acquire_owned().map_err(|_| {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                ApiKeyAuthError::ConcurrentLimit
            })?),
            None => None,
        };

        let now = unix_timestamp();
        let mut window = self
            .window
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if window.minute_started == 0 || now.saturating_sub(window.minute_started) >= 60 {
            window.minute_started = now;
            window.minute_requests = 0;
        }
        let day_number = now / 86_400;
        if window.day_number != day_number {
            window.day_number = day_number;
            window.daily_requests = 0;
        }
        if policy
            .requests_per_minute
            .is_some_and(|limit| window.minute_requests >= limit)
        {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(ApiKeyAuthError::RequestsPerMinute);
        }
        if policy
            .daily_request_quota
            .is_some_and(|limit| window.daily_requests >= limit)
        {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(ApiKeyAuthError::DailyQuota);
        }
        window.minute_requests = window.minute_requests.saturating_add(1);
        window.daily_requests = window.daily_requests.saturating_add(1);
        drop(window);
        self.requests.fetch_add(1, Ordering::Relaxed);
        self.last_used_at.store(now, Ordering::Relaxed);
        Ok(permit)
    }

    fn snapshot(&self) -> RuntimeSnapshot {
        let window = self
            .window
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        RuntimeSnapshot {
            requests: self.requests.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            last_used_at: self.last_used_at.load(Ordering::Relaxed),
            minute_started: window.minute_started,
            minute_requests: window.minute_requests,
            day_number: window.day_number,
            daily_requests: window.daily_requests,
        }
    }

    fn view(&self) -> ApiKeyUsageView {
        let snapshot = self.snapshot();
        let in_flight = self
            .semaphore
            .as_ref()
            .zip(self.max_concurrent)
            .map(|(semaphore, max)| max.saturating_sub(semaphore.available_permits()))
            .unwrap_or(0);
        ApiKeyUsageView {
            requests: snapshot.requests,
            rejected: snapshot.rejected,
            last_used_at: (snapshot.last_used_at > 0).then_some(snapshot.last_used_at),
            in_flight,
            minute_requests: snapshot.minute_requests,
            daily_requests: snapshot.daily_requests,
        }
    }
}

pub fn registry_path(config_path: &Path) -> PathBuf {
    let name = config_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("opencode2api");
    config_path.with_file_name(format!("{name}.api-keys.json"))
}

pub fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn is_web_search_tool(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "web_search" | "websearch" | "web_fetch" | "webfetch"
    )
}

fn import_legacy_records(config: &BridgeConfig, file: &mut ApiKeyRegistryFile) {
    let suppressed: HashSet<&str> = file
        .suppressed_legacy_hashes
        .iter()
        .map(String::as_str)
        .collect();
    let mut existing_hashes: HashSet<String> = file
        .keys
        .iter()
        .map(|record| record.secret_hash.clone())
        .collect();
    let Some(tokens) = &config.auth_tokens else {
        return;
    };
    for (index, token) in tokens.iter().enumerate() {
        let digest = hash_token(token.expose());
        if suppressed.contains(digest.as_str()) || existing_hashes.contains(&digest) {
            continue;
        }
        let id = format!("legacy_{}", &digest[..12]);
        let mut policy = ApiKeyPolicy::default();
        policy.permissions.shell = config.shell_policy.kind() != "disabled";
        existing_hashes.insert(digest.clone());
        file.keys.push(ApiKeyRecord {
            id,
            name: format!("Legacy key {}", index + 1),
            description: Some(
                "Imported from auth_tokens in the main TOML configuration.".to_string(),
            ),
            fingerprint: fingerprint(token.expose()),
            secret_hash: digest,
            status: ApiKeyStatus::Active,
            environment: "production".to_string(),
            created_at: unix_timestamp(),
            expires_at: None,
            source: ApiKeySource::Legacy,
            policy,
        });
    }
}

fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn default_true() -> bool {
    true
}

fn registry_schema_version() -> u32 {
    REGISTRY_SCHEMA_VERSION
}

fn default_environment() -> String {
    "production".to_string()
}

fn normalize_environment(value: &str) -> String {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "development" | "staging" | "production" => value,
        "dev" => "development".to_string(),
        "stage" => "staging".to_string(),
        _ if value.is_empty() => "production".to_string(),
        _ => value.chars().take(32).collect(),
    }
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn nonzero<T>(value: Option<T>) -> Option<T>
where
    T: PartialEq + From<u8>,
{
    value.filter(|value| *value != T::from(0))
}

// ---------------------------------------------------------------------------
// Legacy auth_tokens helpers kept for CLI/config backward compatibility.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ApiKeyMetadata {
    pub index: usize,
    pub fingerprint: String,
    pub length: usize,
    pub active: bool,
}

pub fn generate_api_keys(
    count: usize,
    random_bytes: usize,
    prefix: &str,
) -> Result<Vec<String>, ApiKeyError> {
    if !(1..=20).contains(&count) {
        return Err(ApiKeyError::InvalidCount);
    }
    if !(16..=64).contains(&random_bytes) {
        return Err(ApiKeyError::InvalidSize);
    }

    (0..count)
        .map(|_| {
            crate::infrastructure::random::secure_random_hex(random_bytes)
                .map(|random| format!("{prefix}{random}"))
                .map_err(|error| ApiKeyError::Random(error.to_string()))
        })
        .collect()
}

pub fn merge_auth_tokens(
    existing: &str,
    generated: &[String],
    replace: bool,
) -> Result<String, ApiKeyError> {
    let document = parse_document(existing)?;
    let mut tokens = if replace {
        Vec::new()
    } else {
        existing_tokens(document.get("auth_tokens"))
    };
    tokens.extend(generated.iter().cloned());
    normalize_tokens(&mut tokens);
    render_auth_tokens(document, &tokens)
}

pub fn remove_auth_tokens(existing: &str, indices: &[usize]) -> Result<String, ApiKeyError> {
    if indices.is_empty() {
        return Err(ApiKeyError::InvalidSelection);
    }
    let document = parse_document(existing)?;
    let tokens = existing_tokens(document.get("auth_tokens"));
    let selected: HashSet<usize> = indices.iter().copied().collect();
    if !selected.iter().any(|index| *index < tokens.len()) {
        return Err(ApiKeyError::InvalidSelection);
    }
    let remaining = tokens
        .into_iter()
        .enumerate()
        .filter_map(|(index, token)| (!selected.contains(&index)).then_some(token))
        .collect::<Vec<_>>();
    render_auth_tokens(document, &remaining)
}

pub fn save_auth_tokens(
    path: &Path,
    generated: &[String],
    replace: bool,
) -> Result<(), ApiKeyError> {
    update_file(path, |existing| {
        merge_auth_tokens(existing, generated, replace)
    })
}

pub fn revoke_auth_tokens(path: &Path, indices: &[usize]) -> Result<(), ApiKeyError> {
    update_file(path, |existing| remove_auth_tokens(existing, indices))
}

pub fn load_auth_tokens(path: &Path) -> Result<Vec<String>, ApiKeyError> {
    let store = AtomicFileStore;
    let existing = match store.read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(ApiKeyError::Read(error)),
    };
    let document = parse_document(&existing)?;
    Ok(existing_tokens(document.get("auth_tokens")))
}

pub fn key_inventory(tokens: &[String], active_key: Option<&str>) -> Vec<ApiKeyMetadata> {
    tokens
        .iter()
        .enumerate()
        .map(|(index, token)| ApiKeyMetadata {
            index,
            fingerprint: fingerprint(token),
            length: token.chars().count(),
            active: active_key.is_some_and(|active| active == token),
        })
        .collect()
}

fn update_file<F>(path: &Path, operation: F) -> Result<(), ApiKeyError>
where
    F: FnOnce(&str) -> Result<String, ApiKeyError>,
{
    let store = AtomicFileStore;
    let existing = match store.read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(ApiKeyError::Read(error)),
    };
    let updated = operation(&existing)?;
    store
        .atomic_write(path, updated.as_bytes(), true)
        .map_err(ApiKeyError::Write)
}

fn parse_document(existing: &str) -> Result<DocumentMut, ApiKeyError> {
    if existing.trim().is_empty() {
        let mut document = DocumentMut::new();
        document["schema_version"] = value(1);
        Ok(document)
    } else {
        Ok(existing.parse::<DocumentMut>()?)
    }
}

fn render_auth_tokens(mut document: DocumentMut, tokens: &[String]) -> Result<String, ApiKeyError> {
    let mut array = Array::new();
    for token in tokens {
        array.push(token.as_str());
    }
    document["auth_tokens"] = Item::Value(Value::Array(array));
    let rendered = document.to_string();
    toml::from_str::<TomlConfig>(&rendered).map_err(ApiKeyError::Validation)?;
    Ok(rendered)
}

fn normalize_tokens(tokens: &mut Vec<String>) {
    let mut seen = HashSet::new();
    tokens.retain(|token| !token.trim().is_empty() && seen.insert(token.clone()));
}

fn fingerprint(token: &str) -> String {
    let chars = token.chars().collect::<Vec<_>>();
    if chars.len() <= 18 {
        return "••••••••".to_string();
    }
    let prefix = chars.iter().take(14).collect::<String>();
    let suffix = chars
        .iter()
        .skip(chars.len().saturating_sub(6))
        .collect::<String>();
    format!("{prefix}…{suffix}")
}

fn existing_tokens(item: Option<&Item>) -> Vec<String> {
    let Some(value) = item.and_then(Item::as_value) else {
        return Vec::new();
    };
    if let Some(array) = value.as_array() {
        return array
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect();
    }
    value
        .as_str()
        .map(|csv| {
            csv.split(',')
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BridgeConfig, ManagementConfig};
    use std::fs;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "opencode2api-api-key-{name}-{}-{}",
            std::process::id(),
            unix_timestamp()
        ))
    }

    #[test]
    fn generated_keys_have_prefix_entropy_and_are_distinct() {
        let keys = generate_api_keys(2, 32, "sk-oc2-").unwrap();
        assert_eq!(keys.len(), 2);
        assert!(keys.iter().all(|key| key.starts_with("sk-oc2-")));
        assert!(keys.iter().all(|key| key.len() == 7 + 64));
        assert_ne!(keys[0], keys[1]);
    }

    #[test]
    fn merge_preserves_existing_tokens_and_comments() {
        let existing = "# retained comment\nschema_version = 1\nauth_tokens = [\"old\"]\n";
        let merged = merge_auth_tokens(existing, &["new".to_string()], false).unwrap();
        assert!(merged.contains("# retained comment"));
        let parsed: TomlConfig = toml::from_str(&merged).unwrap();
        assert_eq!(
            parsed.auth_tokens.unwrap().into_vec(),
            vec!["old".to_string(), "new".to_string()]
        );
    }

    #[test]
    fn replace_discards_existing_tokens() {
        let merged = merge_auth_tokens(
            "schema_version = 1\nauth_tokens = \"old-a,old-b\"\n",
            &["new".to_string()],
            true,
        )
        .unwrap();
        let parsed: TomlConfig = toml::from_str(&merged).unwrap();
        assert_eq!(parsed.auth_tokens.unwrap().into_vec(), vec!["new"]);
    }

    #[test]
    fn revoke_preserves_comments_and_removes_selected_index() {
        let existing =
            "# keep me\nschema_version = 1\nauth_tokens = [\"one\", \"two\", \"three\"]\n";
        let updated = remove_auth_tokens(existing, &[1]).unwrap();
        assert!(updated.contains("# keep me"));
        let parsed: TomlConfig = toml::from_str(&updated).unwrap();
        assert_eq!(
            parsed.auth_tokens.unwrap().into_vec(),
            vec!["one".to_string(), "three".to_string()]
        );
    }

    #[test]
    fn inventory_is_secret_safe_and_marks_active_key() {
        let tokens = vec![
            "sk-oc2-111111111111111111111111".to_string(),
            "sk-oc2-222222222222222222222222".to_string(),
        ];
        let inventory = key_inventory(&tokens, Some(&tokens[1]));
        assert_eq!(inventory.len(), 2);
        assert!(!inventory[0]
            .fingerprint
            .contains("111111111111111111111111"));
        assert!(!inventory[0].active);
        assert!(inventory[1].active);
    }

    #[tokio::test]
    async fn managed_key_is_hashed_persisted_and_hot_authenticates() {
        let root = temp_path("registry");
        fs::create_dir_all(&root).unwrap();
        let config_path = root.join("config.toml");
        let config = BridgeConfig {
            auth_tokens: None,
            management: ManagementConfig {
                config_path: config_path.clone(),
                ..BridgeConfig::default().management
            },
            ..Default::default()
        };
        let store = AtomicFileStore;
        let mut registry = ApiKeyRegistry::load(&config, &store).unwrap();
        let (view, secret) = registry
            .create(
                "Mobile App".to_string(),
                None,
                "production".to_string(),
                None,
                ApiKeyPolicy::default(),
                16,
            )
            .unwrap();
        registry.persist(&store).unwrap();
        let persisted = fs::read_to_string(registry.path()).unwrap();
        assert!(!persisted.contains(&secret));
        assert!(persisted.contains(&view.id));
        let admission = registry
            .match_secret(&secret, "/v1/messages")
            .unwrap()
            .admit()
            .await
            .unwrap();
        assert_eq!(admission.client.key_id, view.id);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn policy_rejects_disallowed_model_and_clamps_when_configured() {
        let mut policy = ApiKeyPolicy {
            allowed_models: vec!["opencode/allowed".to_string()],
            max_output_tokens: Some(4096),
            limit_action: LimitAction::Clamp,
            ..Default::default()
        };
        policy.normalize();
        assert!(policy
            .resolve_model(Some("opencode/blocked"), None, "fallback")
            .is_err());
        assert_eq!(
            policy.enforce_output_tokens(Some(9000)).unwrap(),
            Some(4096)
        );
    }
}
